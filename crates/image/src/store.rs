//! Local image store: unpacked rootfs + metadata per image.

use mvm_common::{DataDir, Error, ImageConfig, ImageInfo, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use crate::reference::ImageReference;
use crate::registry::{PullEvent, PullOutcome, RegistryClient};

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
    /// Per-store-key pull serialization (concurrent pulls of the same
    /// reference would race on the store directory).
    pull_locks: Arc<(Mutex<HashSet<String>>, Condvar)>,
}

/// Holds one key in the pull-lock set; released on drop.
struct PullGuard {
    locks: Arc<(Mutex<HashSet<String>>, Condvar)>,
    key: String,
}

impl Drop for PullGuard {
    fn drop(&mut self) {
        let (set, cv) = &*self.locks;
        set.lock().unwrap().remove(&self.key);
        cv.notify_all();
    }
}

impl ImageStore {
    pub fn new(data_dir: DataDir) -> Result<Self> {
        data_dir.ensure()?;
        Ok(Self {
            data_dir,
            client: RegistryClient::new()?,
            pull_locks: Arc::new((Mutex::new(HashSet::new()), Condvar::new())),
        })
    }

    fn lock_pull(&self, key: &str) -> PullGuard {
        let (set, cv) = &*self.pull_locks;
        let mut held = set.lock().unwrap();
        while held.contains(key) {
            held = cv.wait(held).unwrap();
        }
        held.insert(key.to_string());
        PullGuard {
            locks: self.pull_locks.clone(),
            key: key.to_string(),
        }
    }

    /// Pull an image. Skips the download when the stored copy already
    /// matches the registry digest; otherwise downloads into a staging dir
    /// and swaps it in atomically, so a failed pull never destroys the
    /// existing image. Concurrent pulls of the same reference serialize.
    pub fn pull(&self, reference: &str, on_event: impl FnMut(PullEvent)) -> Result<ImageInfo> {
        let reference = ImageReference::parse(reference)?;
        let key = reference.store_key();
        let _guard = self.lock_pull(&key);

        let dir = self.data_dir.image_dir(&key);
        let existing_digest = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|data| serde_json::from_str::<ImageMeta>(&data).ok())
            .map(|meta| meta.digest);

        // Stage under a dot-dir (list() skips those) in the same fs.
        let staging = self.data_dir.images_dir().join(format!(".pulling-{key}"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let rootfs = staging.join("rootfs");

        let outcome = self
            .client
            .pull(&reference, &rootfs, existing_digest.as_deref(), on_event);
        let pulled = match outcome {
            Ok(PullOutcome::Pulled(pulled)) => pulled,
            Ok(PullOutcome::UpToDate { .. }) => {
                let _ = std::fs::remove_dir_all(&staging);
                return self.load_stored(&dir).map(|img| img.info);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };

        let meta = ImageMeta {
            reference: reference.familiar(),
            digest: pulled.digest,
            size: pulled.size,
            created_at: chrono::Utc::now(),
            config: pulled.config,
        };
        std::fs::write(
            staging.join("meta.json"),
            serde_json::to_string_pretty(&meta)?,
        )?;

        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::rename(&staging, &dir)?;

        Ok(ImageInfo {
            reference: meta.reference,
            digest: meta.digest,
            size: meta.size,
            created_at: meta.created_at,
        })
    }

    /// Load an OCI image layout archive (`.tar`) into the store under `name`.
    /// The archive is unpacked into a staging dir and swapped in atomically,
    /// mirroring `pull`; a failed load never destroys an existing image.
    pub fn load(
        &self,
        name: &str,
        archive_path: &Path,
        on_event: impl FnMut(PullEvent),
    ) -> Result<ImageInfo> {
        let reference = ImageReference::parse(name)?;
        let key = reference.store_key();
        let _guard = self.lock_pull(&key);

        let dir = self.data_dir.image_dir(&key);
        let staging = self.data_dir.images_dir().join(format!(".loading-{key}"));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let rootfs = staging.join("rootfs");

        let mut on_event = on_event;
        on_event(PullEvent::Manifest {
            reference: reference.familiar(),
        });

        let loaded = match crate::load::unpack_oci_archive(archive_path, &rootfs, &mut on_event) {
            Ok(loaded) => loaded,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        };

        let meta = ImageMeta {
            reference: reference.familiar(),
            digest: loaded.digest,
            size: loaded.size,
            created_at: chrono::Utc::now(),
            config: loaded.config,
        };
        std::fs::write(
            staging.join("meta.json"),
            serde_json::to_string_pretty(&meta)?,
        )?;

        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::rename(&staging, &dir)?;

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
                return self.load_stored(&dir);
            }
        }
        // Fall back to matching by familiar reference prefix.
        let matches: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|i| {
                i.reference == reference || i.reference.starts_with(&format!("{reference}:"))
            })
            .collect();
        match matches.len() {
            1 => {
                let key = ImageReference::parse(&matches[0].reference)?.store_key();
                self.load_stored(&self.data_dir.image_dir(&key))
            }
            0 => Err(Error::ImageNotFound(reference.to_string())),
            _ => Err(Error::Image(format!(
                "ambiguous image reference '{reference}'"
            ))),
        }
    }

    fn load_stored(&self, dir: &Path) -> Result<StoredImage> {
        let meta: ImageMeta =
            serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json"))?)?;
        Ok(StoredImage {
            info: ImageInfo {
                reference: meta.reference,
                digest: meta.digest,
                size: meta.size,
                created_at: meta.created_at,
            },
            config: meta.config,
            rootfs: dir.join("rootfs"),
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
            // Skip staging dirs (".pulling-*") and stray files.
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
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
}
