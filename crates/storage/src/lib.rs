//! Per-sandbox writable filesystem layers.
//!
//! Two drivers (both served to the guest over virtiofs; the daemon's userns
//! mode gives virtiofs full chown semantics rootless):
//!  - `overlay`: OverlayFS (image = lower, per-sandbox upper); default for
//!    root and for namespace-root (rootless userns mode).
//!  - `copy`: full recursive copy of the image rootfs; fallback when
//!    overlay is unavailable.

use mvm_common::{DataDir, Error, Result, SandboxId};
use std::path::{Path, PathBuf};

mod copy;
mod overlay;

pub use copy::CopyDriver;
pub use overlay::OverlayDriver;

/// What the caller gets back after preparing a sandbox filesystem.
pub struct PreparedRootfs {
    /// Path to pass to libkrun as the (virtiofs) guest root.
    pub rootfs: PathBuf,
}

/// Storage driver interface.
pub trait StorageDriver: Send + Sync {
    fn name(&self) -> &'static str;
    /// Create a writable root for `id` based on `image_rootfs`.
    fn create(&self, id: &SandboxId, image_rootfs: &Path) -> Result<PreparedRootfs>;
    /// Copy `from`'s *current disk* into the (already created) sandbox dir
    /// `to` — `mvm clone --fork`. Each driver duplicates what it would have
    /// produced at the source's last boot (overlay: the persisted upper
    /// layer; copy: the whole rootfs). A source that never booted has no
    /// state to carry; the target is left pristine.
    fn duplicate(&self, from: &SandboxId, to: &SandboxId) -> Result<()>;
    /// Tear down everything created for `id`.
    fn destroy(&self, id: &SandboxId) -> Result<()>;
}

/// Pick the default driver: $MVM_STORAGE_DRIVER, else overlay whenever the
/// host supports it (real root, or namespace-root in the daemon's rootless
/// userns mode), else copy.
pub fn default_driver(data_dir: DataDir) -> Box<dyn StorageDriver> {
    let choice = std::env::var("MVM_STORAGE_DRIVER").unwrap_or_default();
    match choice.as_str() {
        "copy" => {
            tracing::debug!("storage: copy driver (MVM_STORAGE_DRIVER=copy)");
            return Box::new(CopyDriver::new(data_dir));
        }
        "overlay" => {
            tracing::debug!("storage: overlay driver (MVM_STORAGE_DRIVER=overlay)");
            return Box::new(OverlayDriver::new(data_dir));
        }
        _ => {}
    }
    if is_root() && data_dir.ensure().is_ok() && OverlayDriver::probe(&data_dir) {
        tracing::debug!("storage: overlay driver (root, probe ok)");
        Box::new(OverlayDriver::new(data_dir))
    } else {
        tracing::debug!("storage: copy driver (not root or overlay probe failed)");
        Box::new(CopyDriver::new(data_dir))
    }
}

pub(crate) fn storage_err(msg: impl Into<String>) -> Error {
    Error::Storage(msg.into())
}

/// Recursive copy preserving modes and symlinks. Attempts reflink for
/// regular files (btrfs/xfs), falling back to a plain copy. As root
/// (including userns namespace-root) ownership is preserved too.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
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
    if is_root() {
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
        let preserve = if is_root() {
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
    if !is_root() {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    if let Ok(c_path) = std::ffi::CString::new(dst.as_os_str().as_encoded_bytes()) {
        unsafe { libc::lchown(c_path.as_ptr(), src_meta.uid(), src_meta.gid()) };
    }
}

/// One-shot CoW clone of a whole directory tree (APFS `clonefile(2)`), when
/// the filesystem supports it. `dst` must not exist. Returns `Ok(true)` on
/// success, `Ok(false)` when the fs can't do it (caller falls back to a
/// per-file copy). Compared with walking the tree and `clonefile`-ing each
/// file, this turns a multi-second copy of a large image into a single
/// near-instant clone.
#[cfg(target_os = "macos")]
pub(crate) fn clone_dir(src: &Path, dst: &Path) -> Result<bool> {
    use std::ffi::CString;
    let (Ok(s), Ok(d)) = (
        CString::new(src.as_os_str().as_encoded_bytes()),
        CString::new(dst.as_os_str().as_encoded_bytes()),
    ) else {
        return Ok(false);
    };
    Ok(unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } == 0)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn clone_dir(_src: &Path, _dst: &Path) -> Result<bool> {
    Ok(false)
}

pub(crate) fn is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}
