//! Minimal OCI/Docker registry HTTP client (pull path only).

use mvm_common::{ImageConfig, Result};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use crate::reference::{ImageReference, RefKind};
use crate::{img_err, ImgResult};

const ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.docker.distribution.manifest.v2+json"
);

/// Progress events emitted while pulling.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "stage", rename_all = "lowercase")]
pub enum PullEvent {
    Manifest { reference: String },
    Config { digest: String },
    LayerStart { digest: String, size: u64 },
    LayerProgress { digest: String, downloaded: u64 },
    LayerDone { digest: String },
    Unpacking { digest: String },
    Done { digest: String },
    UpToDate { digest: String },
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default, rename = "mediaType")]
    media_type: String,
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize, Default)]
struct Platform {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
}

#[derive(Debug, Deserialize)]
struct Index {
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigBlob {
    #[serde(default)]
    config: ConfigInner,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ConfigInner {
    #[serde(default, rename = "Env")]
    env: Vec<String>,
    #[serde(default, rename = "Entrypoint")]
    entrypoint: Vec<String>,
    #[serde(default, rename = "Cmd")]
    cmd: Vec<String>,
    #[serde(default, rename = "WorkingDir")]
    workdir: String,
    #[serde(default, rename = "User")]
    user: String,
}

/// Parse an OCI/Docker image config blob into the shared `ImageConfig`.
/// Shared by the registry pull path and `mvm load` (OCI layout archives
/// store the exact same config JSON as a content-addressed blob).
pub(crate) fn image_config_from_bytes(bytes: &[u8]) -> ImgResult<ImageConfig> {
    let config_blob: ConfigBlob = serde_json::from_slice(bytes)?;
    Ok(ImageConfig {
        env: config_blob.config.env,
        entrypoint: config_blob.config.entrypoint,
        cmd: config_blob.config.cmd,
        workdir: if config_blob.config.workdir.is_empty() {
            None
        } else {
            Some(config_blob.config.workdir)
        },
        user: if config_blob.config.user.is_empty() {
            None
        } else {
            Some(config_blob.config.user)
        },
    })
}

/// Blocking registry client with Bearer-token challenge auth.
pub struct RegistryClient {
    http: reqwest::blocking::Client,
    tokens: Mutex<std::collections::HashMap<String, String>>,
}

/// Result of a pull attempt.
pub enum PullOutcome {
    Pulled(PulledImage),
    /// The stored copy already matches the registry's manifest digest.
    UpToDate {
        digest: String,
    },
}

/// Everything pulled for one image, unpacked on disk.
pub struct PulledImage {
    pub digest: String,
    pub config: ImageConfig,
    pub size: u64,
}

impl RegistryClient {
    pub fn new() -> ImgResult<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("mvm/0.1")
            .build()
            .map_err(|e| img_err(format!("http client: {e}")))?;
        Ok(Self {
            http,
            tokens: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Pull `reference`, unpack layers into `dest_rootfs`, and report progress
    /// through `on_event`. When the resolved manifest digest equals
    /// `skip_if_digest`, nothing is downloaded and `UpToDate` is returned.
    pub fn pull(
        &self,
        reference: &ImageReference,
        dest_rootfs: &Path,
        skip_if_digest: Option<&str>,
        mut on_event: impl FnMut(PullEvent),
    ) -> Result<PullOutcome> {
        on_event(PullEvent::Manifest {
            reference: reference.to_string(),
        });

        // 1. Manifest (resolving through an index if needed).
        let (manifest, manifest_digest) = self.fetch_manifest_resolved(reference)?;
        let digest = match &reference.reference {
            RefKind::Digest(d) if manifest.media_type.is_empty() => d.clone(),
            _ => manifest_digest,
        };

        if skip_if_digest == Some(digest.as_str()) {
            on_event(PullEvent::UpToDate {
                digest: digest.clone(),
            });
            return Ok(PullOutcome::UpToDate { digest });
        }

        // 2. Config blob.
        on_event(PullEvent::Config {
            digest: manifest.config.digest.clone(),
        });
        let config_bytes = self.fetch_blob(reference, &manifest.config.digest)?;
        let config = image_config_from_bytes(&config_bytes)?;

        // 3. Layers.
        std::fs::create_dir_all(dest_rootfs)?;
        let mut total_size = 0u64;
        for layer in &manifest.layers {
            if layer.media_type.contains("nondistributable") || layer.media_type.contains("foreign")
            {
                tracing::warn!("skipping foreign layer {}", layer.digest);
                continue;
            }
            on_event(PullEvent::LayerStart {
                digest: layer.digest.clone(),
                size: layer.size,
            });
            let blob = self.fetch_blob_streaming(reference, layer, &mut |n| {
                on_event(PullEvent::LayerProgress {
                    digest: layer.digest.clone(),
                    downloaded: n,
                })
            })?;
            on_event(PullEvent::Unpacking {
                digest: layer.digest.clone(),
            });
            crate::unpack::unpack_layer(&blob[..], &layer.media_type, dest_rootfs)?;
            total_size += blob.len() as u64;
            on_event(PullEvent::LayerDone {
                digest: layer.digest.clone(),
            });
        }

        on_event(PullEvent::Done {
            digest: digest.clone(),
        });
        Ok(PullOutcome::Pulled(PulledImage {
            digest,
            config,
            size: total_size,
        }))
    }

    /// Fetch a manifest; if it is an index/list, resolve to the platform
    /// manifest for this host.
    fn fetch_manifest_resolved(&self, reference: &ImageReference) -> ImgResult<(Manifest, String)> {
        let (bytes, content_type) = self.get_manifest_raw(reference, reference.ref_str())?;

        if is_index(&content_type, &bytes) {
            let index: Index = serde_json::from_slice(&bytes)
                .map_err(|e| img_err(format!("bad image index: {e}")))?;
            let (arch, os) = host_platform();
            let desc = index
                .manifests
                .iter()
                .find(|m| {
                    m.platform
                        .as_ref()
                        .map(|p| p.architecture == arch && p.os == os)
                        .unwrap_or(false)
                })
                .or_else(|| index.manifests.first())
                .ok_or_else(|| img_err("no suitable platform manifest in index"))?;
            let (bytes, _) = self.get_manifest_raw(reference, &desc.digest)?;
            let digest = desc.digest.clone();
            let manifest: Manifest = serde_json::from_slice(&bytes)
                .map_err(|e| img_err(format!("bad platform manifest: {e}")))?;
            return Ok((manifest, digest));
        }

        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| img_err(format!("bad image manifest: {e}")))?;
        // Digest of the manifest we fetched (for tag refs, compute it).
        let digest = match &reference.reference {
            RefKind::Digest(d) => d.clone(),
            RefKind::Tag(_) => format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
        };
        let _ = content_type;
        Ok((manifest, digest))
    }

    fn get_manifest_raw(
        &self,
        reference: &ImageReference,
        ref_str: &str,
    ) -> ImgResult<(Vec<u8>, String)> {
        let url = format!(
            "https://{}/v2/{}/manifests/{}",
            reference.registry, reference.repository, ref_str
        );
        let resp = self.send_with_auth(&url, &[("Accept", ACCEPT)])?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = resp
            .bytes()
            .map_err(|e| img_err(format!("manifest body: {e}")))?;
        Ok((bytes.to_vec(), content_type))
    }

    fn fetch_blob(&self, reference: &ImageReference, digest: &str) -> ImgResult<Vec<u8>> {
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            reference.registry, reference.repository, digest
        );
        let resp = self.send_with_auth(&url, &[])?;
        let bytes = resp
            .bytes()
            .map_err(|e| img_err(format!("blob body: {e}")))?;
        verify_digest(digest, &bytes)?;
        Ok(bytes.to_vec())
    }

    fn fetch_blob_streaming(
        &self,
        reference: &ImageReference,
        desc: &Descriptor,
        mut on_progress: impl FnMut(u64),
    ) -> ImgResult<Vec<u8>> {
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            reference.registry, reference.repository, desc.digest
        );
        let mut resp = self.send_with_auth(&url, &[])?;
        let mut buf = Vec::with_capacity(desc.size as usize);
        let mut chunk = [0u8; 1 << 16];
        let mut downloaded = 0u64;
        loop {
            let n = resp
                .read(&mut chunk)
                .map_err(|e| img_err(format!("blob read: {e}")))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            downloaded += n as u64;
            on_progress(downloaded);
        }
        verify_digest(&desc.digest, &buf)?;
        Ok(buf)
    }

    /// GET with Bearer challenge handling (anonymous token flow).
    fn send_with_auth(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> ImgResult<reqwest::blocking::Response> {
        let token_key = token_cache_key(url);
        let cached = self.tokens.lock().unwrap().get(&token_key).cloned();

        let build = |token: Option<&str>| {
            let mut req = self.http.get(url);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            req
        };

        let resp = build(cached.as_deref())
            .send()
            .map_err(|e| img_err(format!("request {url}: {e}")))?;

        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return error_for_status(resp);
        }

        // Parse the WWW-Authenticate challenge and fetch an anonymous token.
        let challenge = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let token = self.fetch_bearer_token(&challenge)?;
        self.tokens.lock().unwrap().insert(token_key, token.clone());

        let resp = build(Some(&token))
            .send()
            .map_err(|e| img_err(format!("request {url}: {e}")))?;
        error_for_status(resp)
    }

    fn fetch_bearer_token(&self, challenge: &str) -> ImgResult<String> {
        // WWW-Authenticate: Bearer realm="...",service="...",scope="..."
        let params = parse_auth_params(challenge);
        let realm = params
            .get("realm")
            .ok_or_else(|| img_err(format!("no realm in auth challenge: {challenge}")))?;
        let mut url = format!("{realm}?");
        if let Some(service) = params.get("service") {
            url.push_str(&format!("service={service}&"));
        }
        if let Some(scope) = params.get("scope") {
            url.push_str(&format!("scope={scope}&"));
        }
        #[derive(Deserialize)]
        struct TokenResp {
            token: Option<String>,
            access_token: Option<String>,
        }
        let resp: TokenResp = self
            .http
            .get(&url)
            .send()
            .and_then(|r| r.json())
            .map_err(|e| img_err(format!("token request: {e}")))?;
        resp.token
            .or(resp.access_token)
            .ok_or_else(|| img_err("token response contained no token"))
    }
}

fn token_cache_key(url: &str) -> String {
    // Cache per registry+repo (the scope is embedded in the URL path).
    url.rsplit_once("/manifests/")
        .or_else(|| url.rsplit_once("/blobs/"))
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| url.to_string())
}

fn error_for_status(resp: reqwest::blocking::Response) -> ImgResult<reqwest::blocking::Response> {
    let status = resp.status();
    if status.is_success() || status.is_redirection() {
        Ok(resp)
    } else {
        let body = resp.text().unwrap_or_default();
        Err(img_err(format!("registry returned {status}: {body}")))
    }
}

fn parse_auth_params(challenge: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let challenge = challenge
        .trim_start_matches("Bearer")
        .trim_start_matches("bearer");
    for part in challenge.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    map
}

fn is_index(content_type: &str, body: &[u8]) -> bool {
    if content_type.contains("manifest.list") || content_type.contains("image.index") {
        return true;
    }
    // Some registries serve a bare JSON content type; sniff.
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        let mt = v.get("mediaType").and_then(|m| m.as_str()).unwrap_or("");
        return mt.contains("manifest.list") || mt.contains("image.index");
    }
    false
}

pub(crate) fn verify_digest(digest: &str, bytes: &[u8]) -> ImgResult<()> {
    if let Some(expected) = digest.strip_prefix("sha256:") {
        let actual = hex::encode(Sha256::digest(bytes));
        if actual != expected {
            return Err(img_err(format!(
                "digest mismatch: expected {digest}, got sha256:{actual}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn host_platform() -> (String, String) {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    (arch.to_string(), "linux".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auth_challenge() {
        let c = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#;
        let p = parse_auth_params(c);
        assert_eq!(p.get("realm").unwrap(), "https://auth.docker.io/token");
        assert_eq!(p.get("service").unwrap(), "registry.docker.io");
    }
}
