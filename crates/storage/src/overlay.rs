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

    /// Check that overlay mounts actually work here (real root, or
    /// namespace-root on a kernel with unprivileged overlayfs, >= 5.11).
    pub fn probe(data_dir: &DataDir) -> bool {
        let base = data_dir.root().join(".overlay-probe");
        let _ = std::fs::remove_dir_all(&base);
        let (lower, upper, work, merged) = (
            base.join("lower"),
            base.join("upper"),
            base.join("work"),
            base.join("merged"),
        );
        for d in [&lower, &upper, &work, &merged] {
            if std::fs::create_dir_all(d).is_err() {
                return false;
            }
        }
        let ok = mount_overlay(&lower, &upper, &work, &merged).is_ok();
        if ok {
            let _ = Command::new("umount").arg(&merged).status();
        }
        let _ = std::fs::remove_dir_all(&base);
        ok
    }
}

fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    let status = Command::new("mount")
        .args(["-t", "overlay", "overlay", "-o", &opts])
        .arg(merged)
        .status()
        .map_err(|e| storage_err(format!("spawning mount: {e}")))?;
    if !status.success() {
        return Err(storage_err(format!(
            "overlay mount failed for {}",
            merged.display()
        )));
    }
    Ok(())
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

        // Clear any stale mount, then rebuild work/merged. The upper layer
        // is kept: sandbox filesystem changes persist across stop/start.
        if merged.exists() {
            let _ = Command::new("umount").arg(&merged).status();
        }
        if work.exists() {
            std::fs::remove_dir_all(&work)?;
        }
        std::fs::create_dir_all(&upper)?;
        std::fs::create_dir_all(&work)?;
        std::fs::create_dir_all(&merged)?;

        mount_overlay(image_rootfs, &upper, &work, &merged)?;
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
