//! OCI Image Layout archive reader.
//!
//! Reads a `.tar` produced by `podman save --format oci-archive`, `buildah
//! push oci:`, `skopeo copy oci:`, `oras push --format oci`, etc.:
//!
//! ```text
//! oci-layout             {"imageLayoutVersion": "1.0.0"}
//! index.json             OCI image index
//! blobs/sha256/<hex>     content-addressed config / manifest / layer blobs
//! ```
//!
//! Layers are stored directly as content-addressed files (no nested tars, as
//! docker-archive has), so importing is just: resolve the platform manifest,
//! then unpack each layer blob — the same config parsing and unpack path the
//! registry pull uses.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use mvm_common::{ImageConfig, Result};
use serde::Deserialize;

use crate::registry::{host_platform, image_config_from_bytes, verify_digest};
use crate::unpack::unpack_layer;
use crate::{img_err, ImgResult, PullEvent};

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Debug, Deserialize)]
struct Platform {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(rename = "mediaType", default)]
    #[allow(dead_code)]
    media_type: String,
    config: Descriptor,
    #[serde(default)]
    layers: Vec<Descriptor>,
}

#[derive(Debug, Deserialize)]
struct Index {
    #[serde(default)]
    manifests: Vec<Descriptor>,
}

/// Only the mediaType, to tell an image manifest from a nested index.
#[derive(Debug, Deserialize)]
struct ManifestShell {
    #[serde(rename = "mediaType", default)]
    media_type: String,
}

/// Everything unpacked from one OCI layout archive.
pub struct LoadedImage {
    pub digest: String,
    pub config: ImageConfig,
    pub size: u64,
}

/// Unpack an OCI image layout tar into `dest_rootfs`, reporting progress
/// through `on_event`. Returns the resolved manifest digest, the config, and
/// the total layer size.
pub fn unpack_oci_archive(
    archive_path: &Path,
    dest_rootfs: &Path,
    on_event: &mut dyn FnMut(PullEvent),
) -> Result<LoadedImage> {
    let index_bytes = read_entry(archive_path, "index.json")?;
    let index: Index = serde_json::from_slice(&index_bytes)
        .map_err(|e| img_err(format!("bad oci index.json: {e}")))?;

    let (manifest_digest, manifest) = resolve_manifest(archive_path, &index)?;

    on_event(PullEvent::Config {
        digest: manifest.config.digest.clone(),
    });
    let config_bytes = read_blob(archive_path, &manifest.config.digest)?;
    let config = image_config_from_bytes(&config_bytes)?;

    std::fs::create_dir_all(dest_rootfs)?;
    let mut total_size = 0u64;
    for layer in &manifest.layers {
        on_event(PullEvent::LayerStart {
            digest: layer.digest.clone(),
            size: layer.size,
        });
        let blob = read_blob(archive_path, &layer.digest)?;
        on_event(PullEvent::Unpacking {
            digest: layer.digest.clone(),
        });
        unpack_layer(&blob, &layer.media_type, dest_rootfs)?;
        total_size += blob.len() as u64;
        on_event(PullEvent::LayerDone {
            digest: layer.digest.clone(),
        });
    }

    on_event(PullEvent::Done {
        digest: manifest_digest.clone(),
    });
    Ok(LoadedImage {
        digest: manifest_digest,
        config,
        size: total_size,
    })
}

/// Pick a platform manifest out of an index, resolving nested indexes until an
/// image manifest (with a `config`) is reached.
fn resolve_manifest(path: &Path, index: &Index) -> ImgResult<(String, Manifest)> {
    let mut digest = select_platform(&index.manifests)?;
    loop {
        let bytes = read_blob(path, &digest)?;
        let shell: ManifestShell = serde_json::from_slice(&bytes)
            .map_err(|e| img_err(format!("bad oci manifest {digest}: {e}")))?;
        if shell.media_type.contains("index") {
            let nested: Index = serde_json::from_slice(&bytes)
                .map_err(|e| img_err(format!("bad nested oci index {digest}: {e}")))?;
            digest = select_platform(&nested.manifests)?;
            continue;
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| img_err(format!("bad oci image manifest {digest}: {e}")))?;
        return Ok((digest, manifest));
    }
}

fn select_platform(manifests: &[Descriptor]) -> ImgResult<String> {
    let (arch, os) = host_platform();
    manifests
        .iter()
        .find(|m| {
            m.platform
                .as_ref()
                .map(|p| p.architecture == arch && p.os == os)
                .unwrap_or(false)
        })
        .or_else(|| manifests.first())
        .map(|m| m.digest.clone())
        .ok_or_else(|| img_err("oci index contains no manifests"))
}

/// Read a content-addressed blob, verifying it against its digest (the file
/// name under `blobs/sha256/` *is* the digest, so this is a cheap sanity
/// check matching the pull path's).
fn read_blob(path: &Path, digest: &str) -> ImgResult<Vec<u8>> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| img_err(format!("unsupported blob digest '{digest}'")))?;
    let bytes = read_entry(path, &format!("blobs/sha256/{hex}"))?;
    verify_digest(digest, &bytes)?;
    Ok(bytes)
}

/// Read one entry (by archive-rooted path) fully into memory.
fn read_entry(path: &Path, name: &str) -> ImgResult<Vec<u8>> {
    let mut archive = open_archive(path)?;
    let entries = archive
        .entries()
        .map_err(|e| img_err(format!("tar entries: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| img_err(format!("tar entry: {e}")))?;
        let p = entry
            .path()
            .map_err(|e| img_err(format!("tar path: {e}")))?;
        let p = p.to_string_lossy();
        if p.trim_start_matches("./") == name {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| img_err(format!("read {name}: {e}")))?;
            return Ok(buf);
        }
    }
    Err(img_err(format!("'{name}' not found in archive")))
}

/// Open the archive, transparently gunzipping an outer `.tar.gz`.
fn open_archive(path: &Path) -> ImgResult<tar::Archive<Box<dyn Read>>> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .map_err(|e| img_err(format!("read {}: {e}", path.display())))?;
    let reader: Box<dyn Read> = if magic == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(File::open(path)?))
    } else {
        Box::new(File::open(path)?)
    };
    Ok(tar::Archive::new(reader))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;
    use std::io::Write;

    fn digest_of(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
    }

    /// Build a minimal OCI layout tar: one config blob, one layer, one
    /// manifest, wrapped in an index with no platform (so it is selected as
    /// the only candidate).
    fn build_archive() -> (Vec<u8>, String) {
        let config = serde_json::json!({
            "config": {
                "Env": ["PATH=/usr/bin"],
                "Entrypoint": ["/bin/sh"],
                "Cmd": ["-c", "echo hi"],
                "WorkingDir": "/",
                "User": ""
            }
        });
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = digest_of(&config_bytes);

        let layer_bytes = {
            let mut b = tar::Builder::new(Vec::new());
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "hello.txt", &b"hey"[..]).unwrap();
            b.into_inner().unwrap()
        };
        let layer_digest = digest_of(&layer_bytes);

        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config_bytes.len()
            },
            "layers": [
                {
                    "mediaType": "application/vnd.oci.image.layer.v1.tar",
                    "digest": layer_digest,
                    "size": layer_bytes.len()
                }
            ]
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = digest_of(&manifest_bytes);

        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": [
                {
                    "mediaType": "application/vnd.oci.image.manifest.v1+json",
                    "digest": manifest_digest,
                    "size": manifest_bytes.len()
                }
            ]
        });
        let index_bytes = serde_json::to_vec(&index).unwrap();

        let mut b = tar::Builder::new(Vec::new());
        let mut add = |name: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data).unwrap();
        };
        add("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#);
        add("index.json", &index_bytes);
        add(&format!("blobs/sha256/{}", &config_digest[7..]), &config_bytes);
        add(&format!("blobs/sha256/{}", &manifest_digest[7..]), &manifest_bytes);
        add(&format!("blobs/sha256/{}", &layer_digest[7..]), &layer_bytes);
        let tar = b.into_inner().unwrap();

        (tar, manifest_digest)
    }

    fn write_tmp(tar: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("mvm-load-test-{}.tar", std::process::id()));
        let mut f = File::create(&path).unwrap();
        f.write_all(tar).unwrap();
        path
    }

    #[test]
    fn unpacks_oci_layout_archive() {
        let (tar, manifest_digest) = build_archive();
        let path = write_tmp(&tar);
        let dest = std::env::temp_dir().join(format!("mvm-load-dest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let mut events = Vec::new();
        let loaded =
            unpack_oci_archive(&path, &dest, &mut |e| events.push(e)).expect("unpack archive");

        assert_eq!(loaded.digest, manifest_digest);
        assert_eq!(loaded.config.entrypoint, vec!["/bin/sh"]);
        assert_eq!(loaded.config.env, vec!["PATH=/usr/bin"]);
        assert_eq!(std::fs::read(dest.join("hello.txt")).unwrap(), b"hey");

        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir_all(&dest).unwrap();
    }
}
