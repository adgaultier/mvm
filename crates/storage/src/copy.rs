//! Rootless storage driver: full recursive copy of the image rootfs.
//! Tries reflink copies where the filesystem supports them.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::Path;

use crate::{storage_err, PreparedRootfs, StorageDriver};

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
        std::fs::create_dir_all(&dest)?;
        copy_tree(image_rootfs, &dest)?;
        Ok(PreparedRootfs {
            rootfs: dest,
            root_disk: None,
        })
    }

    fn destroy(&self, id: &SandboxId) -> Result<()> {
        let dir = self.data_dir.sandbox_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// Recursive copy preserving modes and symlinks. Attempts reflink for
/// regular files (btrfs/xfs), falling back to a plain copy.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        let _ = std::fs::set_permissions(dst, meta.permissions());
    } else if meta.is_symlink() {
        let target = std::fs::read_link(src)?;
        if dst.exists() || dst.symlink_metadata().is_ok() {
            std::fs::remove_file(dst)?;
        }
        std::os::unix::fs::symlink(target, dst)?;
    } else if meta.is_file() {
        copy_file(src, dst)?;
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    use std::process::Command;
    // cp --reflink=auto --preserve=mode gives us CoW where possible.
    let status = Command::new("cp")
        .arg("--reflink=auto")
        .arg("--preserve=mode")
        .arg("--")
        .arg(src)
        .arg(dst)
        .status()
        .map_err(|e| storage_err(format!("spawning cp: {e}")))?;
    if !status.success() {
        // Fallback: manual stream copy.
        std::fs::copy(src, dst)?;
        let perms = std::fs::metadata(src)?.permissions();
        let _ = std::fs::set_permissions(dst, perms);
    }
    Ok(())
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
