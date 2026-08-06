//! Rootless storage driver: full recursive copy of the image rootfs.
//! Tries reflink copies where the filesystem supports them.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::Path;

use crate::{PreparedRootfs, StorageDriver};

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
/// regular files (btrfs/xfs), falling back to a plain copy. As root
/// (including userns namespace-root) ownership is preserved too.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        let _ = std::fs::set_permissions(dst, meta.permissions());
        preserve_owner(dst, &meta);
    } else if meta.is_symlink() {
        let target = std::fs::read_link(src)?;
        if dst.exists() || dst.symlink_metadata().is_ok() {
            std::fs::remove_file(dst)?;
        }
        std::os::unix::fs::symlink(target, dst)?;
        preserve_owner(dst, &meta);
    } else if meta.is_file() {
        copy_file(src, dst, &meta)?;
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path, meta: &std::fs::Metadata) -> Result<()> {
    if !fast_copy(src, dst) {
        std::fs::copy(src, dst)?;
    }
    let perms = std::fs::metadata(src)?.permissions();
    let _ = std::fs::set_permissions(dst, perms);
    preserve_owner(dst, meta);
    // chown (by us or the copy) clears setuid/setgid: restore.
    if crate::is_root() {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o6000 != 0 {
            let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// CoW copy where the platform offers one: `cp --reflink=auto` on Linux,
/// clonefile(2) on macOS. False when unavailable (falls back to a stream
/// copy).
fn fast_copy(src: &Path, dst: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let preserve = if crate::is_root() {
            "--preserve=mode,ownership"
        } else {
            "--preserve=mode"
        };
        Command::new("cp")
            .arg("--reflink=auto")
            .arg(preserve)
            .arg("--")
            .arg(src)
            .arg(dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        let (Ok(s), Ok(d)) = (
            CString::new(src.as_os_str().as_encoded_bytes()),
            CString::new(dst.as_os_str().as_encoded_bytes()),
        ) else {
            return false;
        };
        // Fails on non-APFS or across devices; the caller falls back.
        unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) == 0 }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (src, dst);
        false
    }
}

/// Carry the source's uid/gid onto `dst` when running as root (best effort).
fn preserve_owner(dst: &Path, src_meta: &std::fs::Metadata) {
    if !crate::is_root() {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    if let Ok(c_path) = std::ffi::CString::new(dst.as_os_str().as_encoded_bytes()) {
        unsafe { libc::lchown(c_path.as_ptr(), src_meta.uid(), src_meta.gid()) };
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
