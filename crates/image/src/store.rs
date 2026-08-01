//! Local image store: unpacked rootfs + metadata per image.

use mvm_common::{DataDir, Error, ImageConfig, ImageInfo, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::reference::ImageReference;
use crate::registry::{PullEvent, RegistryClient};

const OWNERSHIP_FILE: &str = "ownership.jsonl";

#[derive(Debug, Serialize, Deserialize)]
struct ImageMeta {
    reference: String,
    digest: String,
    size: u64,
    created_at: chrono::DateTime<chrono::Utc>,
    config: ImageConfig,
}

/// Manages unpacked images under `<data>/images/<store-key>/`.
pub struct ImageStore {
    data_dir: DataDir,
    client: RegistryClient,
}

impl ImageStore {
    pub fn new(data_dir: DataDir) -> Result<Self> {
        data_dir.ensure()?;
        Ok(Self {
            data_dir,
            client: RegistryClient::new()?,
        })
    }

    /// Pull an image (re-pull overwrites). Returns its info.
    pub fn pull(
        &self,
        reference: &str,
        on_event: impl FnMut(PullEvent),
    ) -> Result<ImageInfo> {
        let reference = ImageReference::parse(reference)?;
        let key = reference.store_key();
        let dir = self.data_dir.image_dir(&key);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let rootfs = dir.join("rootfs");

        let pulled = self.client.pull(&reference, &rootfs, on_event)?;

        if !pulled.ownership.is_empty() {
            pulled.ownership.save(&dir.join(OWNERSHIP_FILE))?;
        }

        let meta = ImageMeta {
            reference: reference.familiar(),
            digest: pulled.digest,
            size: pulled.size,
            created_at: chrono::Utc::now(),
            config: pulled.config,
        };
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;

        Ok(ImageInfo {
            reference: meta.reference,
            digest: meta.digest,
            size: meta.size,
            created_at: meta.created_at,
        })
    }

    /// Resolve a user-supplied reference to a locally stored image.
    /// Matches exact reference or unique prefix of the reference name.
    pub fn get(&self, reference: &str) -> Result<StoredImage> {
        // Exact match on the parsed key first.
        if let Ok(r) = ImageReference::parse(reference) {
            let dir = self.data_dir.image_dir(&r.store_key());
            if dir.exists() {
                return self.load(&dir);
            }
        }
        // Fall back to matching by familiar reference prefix.
        let matches: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|i| i.reference == reference || i.reference.starts_with(&format!("{reference}:")))
            .collect();
        match matches.len() {
            1 => {
                let key = ImageReference::parse(&matches[0].reference)?.store_key();
                self.load(&self.data_dir.image_dir(&key))
            }
            0 => Err(Error::ImageNotFound(reference.to_string())),
            _ => Err(Error::Image(format!(
                "ambiguous image reference '{reference}'"
            ))),
        }
    }

    fn load(&self, dir: &PathBuf) -> Result<StoredImage> {
        let meta: ImageMeta =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json"))?)?;
        let ownership = dir.join(OWNERSHIP_FILE);
        Ok(StoredImage {
            info: ImageInfo {
                reference: meta.reference,
                digest: meta.digest,
                size: meta.size,
                created_at: meta.created_at,
            },
            config: meta.config,
            rootfs: dir.join("rootfs"),
            ownership: ownership.exists().then_some(ownership),
        })
    }

    pub fn list(&self) -> Result<Vec<ImageInfo>> {
        let mut out = Vec::new();
        let dir = self.data_dir.images_dir();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta_path = entry.path().join("meta.json");
            if let Ok(data) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<ImageMeta>(&data) {
                    out.push(ImageInfo {
                        reference: meta.reference,
                        digest: meta.digest,
                        size: meta.size,
                        created_at: meta.created_at,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.reference.cmp(&b.reference));
        Ok(out)
    }

    pub fn remove(&self, reference: &str) -> Result<()> {
        let img = self.get(reference)?;
        let dir = img
            .rootfs
            .parent()
            .ok_or_else(|| Error::Image("bad store layout".into()))?
            .to_path_buf();
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}

/// An image present in the local store.
pub struct StoredImage {
    pub info: ImageInfo,
    pub config: ImageConfig,
    pub rootfs: PathBuf,
    /// Ownership manifest recorded from tar headers at unpack time. Needed
    /// for block-device roots built from a rootless unpack (chown was
    /// impossible on the host); applying it is idempotent otherwise.
    pub ownership: Option<PathBuf>,
}
