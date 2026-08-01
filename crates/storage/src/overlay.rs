//! OverlayFS storage driver (requires root): image rootfs is the read-only
//! lower layer, each sandbox gets a writable upper + work dir.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::Path;
use std::process::Command;

use crate::{storage_err, PreparedRootfs, StorageDriver};

pub struct OverlayDriver {
    data_dir: DataDir,
}

impl OverlayDriver {
    pub fn new(data_dir: DataDir) -> Self {
        Self { data_dir }
    }
}

impl StorageDriver for OverlayDriver {
    fn name(&self) -> &'static str {
        "overlay"
    }

    fn create(&self, id: &SandboxId, image_rootfs: &Path) -> Result<PreparedRootfs> {
        let base = self.data_dir.sandbox_dir(id);
        let upper = base.join("upper");
        let work = base.join("work");
        let merged = base.join("rootfs");
        if base.exists() {
            let _ = self.destroy(id);
        }
        std::fs::create_dir_all(&upper)?;
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&merged)?;

        let opts = format!(
            "lowerdir={},upperdir={},workdir={}",
            image_rootfs.display(),
            upper.display(),
            work.display()
        );
        let status = Command::new("mount")
            .args(["-t", "overlay", "overlay", "-o", &opts])
            .arg(&merged)
            .status()
            .map_err(|e| storage_err(format!("spawning mount: {e}")))?;
        if !status.success() {
            return Err(storage_err(format!(
                "overlay mount failed for {} (root required)",
                merged.display()
            )));
        }
        Ok(PreparedRootfs {
            rootfs: merged,
            root_disk: None,
        })
    }

    fn destroy(&self, id: &SandboxId) -> Result<()> {
        let base = self.data_dir.sandbox_dir(id);
        let merged = base.join("rootfs");
        if merged.exists() {
            let status = Command::new("umount").arg(&merged).status();
            match status {
                Ok(s) if s.success() => {}
                _ => {
                    // Lazy unmount fallback.
                    let _ = Command::new("umount").arg("-l").arg(&merged).status();
                }
            }
        }
        if base.exists() {
            std::fs::remove_dir_all(&base)?;
        }
        Ok(())
    }
}
