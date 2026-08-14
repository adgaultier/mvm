//! Rootless storage driver: full recursive copy of the image rootfs.
//! Tries reflink copies where the filesystem supports them.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::Path;

use crate::{clone_dir, copy_tree, PreparedRootfs, StorageDriver};

pub struct CopyDriver {
    data_dir: DataDir,
}

impl CopyDriver {
    pub fn new(data_dir: DataDir) -> Self {
        Self { data_dir }
    }

    fn sandbox_root(&self, id: &SandboxId) -> std::path::PathBuf {
        self.data_dir.sandbox_dir(id).join("rootfs")
    }
}

impl StorageDriver for CopyDriver {
    fn name(&self) -> &'static str {
        "copy"
    }

    fn create(&self, id: &SandboxId, image_rootfs: &Path) -> Result<PreparedRootfs> {
        let dest = self.sandbox_root(id);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        // APFS: one CoW clone instead of a per-file walk — large images go
        // from seconds to milliseconds. Off-APFS clonefile fails and we fall
        // back to the recursive copy.
        if !clone_dir(image_rootfs, &dest)? {
            #[cfg(target_os = "macos")]
            tracing::warn!(
                sandbox = %id,
                "copy driver: clonefile rootfs clone unavailable (non-APFS?), falling back to per-file copy"
            );
            std::fs::create_dir_all(&dest)?;
            copy_tree(image_rootfs, &dest)?;
        }
        Ok(PreparedRootfs { rootfs: dest })
    }

    fn duplicate(&self, from: &SandboxId, to: &SandboxId) -> Result<()> {
        let src = self.sandbox_root(from);
        let dst = self.sandbox_root(to);
        if dst.exists() {
            std::fs::remove_dir_all(&dst)?;
        }
        if src.exists() && !clone_dir(&src, &dst)? {
            #[cfg(target_os = "macos")]
            tracing::warn!(
                from = %from,
                to = %to,
                "copy driver: clonefile clone unavailable (non-APFS?), falling back to per-file copy"
            );
            std::fs::create_dir_all(&dst)?;
            copy_tree(&src, &dst)?;
        }
        Ok(())
    }

    fn destroy(&self, id: &SandboxId) -> Result<()> {
        let dir = self.data_dir.sandbox_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_tree_with_symlinks() {
        let base = std::env::temp_dir().join(format!("mvm-copy-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(src.join("etc")).unwrap();
        std::fs::write(src.join("etc/motd"), b"hi").unwrap();
        std::os::unix::fs::symlink("etc/motd", src.join("link")).unwrap();

        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("etc/motd")).unwrap(), b"hi");
        assert!(dst.join("link").symlink_metadata().unwrap().is_symlink());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
