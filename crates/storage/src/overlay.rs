//! OverlayFS storage driver (requires root): image rootfs is the read-only
//! lower layer, each sandbox gets a writable upper + work dir.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::Path;
use std::process::Command;

use crate::{copy_tree, storage_err, PreparedRootfs, StorageDriver};

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
        let ok = match mount_overlay(&lower, &upper, &work, &merged) {
            Ok(()) => {
                let _ = Command::new("umount").arg(&merged).status();
                true
            }
            Err(e) => {
                tracing::warn!("overlay unavailable, falling back to copy driver: {e}");
                false
            }
        };
        let _ = std::fs::remove_dir_all(&base);
        ok
    }
}

fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    let mut opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    // Unprivileged overlay in a user namespace must use user.* xattrs
    // (trusted.* needs init-ns CAP_SYS_ADMIN → EPERM otherwise), and
    // userxattr in turn requires these features off.
    if !mvm_common::is_init_ns_root() {
        opts.push_str(",userxattr,redirect_dir=nofollow,index=off,metacopy=off");
    }
    let out = Command::new("mount")
        .args(["-t", "overlay", "overlay", "-o", &opts])
        .arg(merged)
        .output()
        .map_err(|e| storage_err(format!("spawning mount: {e}")))?;
    if !out.status.success() {
        return Err(storage_err(format!(
            "overlay mount failed for {}: {}",
            merged.display(),
            String::from_utf8_lossy(&out.stderr).trim()
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
        Ok(PreparedRootfs { rootfs: merged })
    }

    fn duplicate(&self, from: &SandboxId, to: &SandboxId) -> Result<()> {
        // The persisted upper layer *is* the current disk: the merged view
        // served at boot is lower(image) + upper, so reusing the same image
        // lower with a copied upper reproduces it. work/ and the mount are
        // rebuilt at the clone's first start.
        let src = self.data_dir.sandbox_dir(from).join("upper");
        let dst = self.data_dir.sandbox_dir(to).join("upper");
        std::fs::create_dir_all(&dst)?;
        if src.exists() {
            copy_tree(&src, &dst)?;
        }
        Ok(())
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
