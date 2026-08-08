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
    /// Tear down everything created for `id`.
    fn destroy(&self, id: &SandboxId) -> Result<()>;
}

/// Pick the default driver: $MVM_STORAGE_DRIVER, else overlay whenever the
/// host supports it (real root, or namespace-root in the daemon's rootless
/// userns mode), else copy.
pub fn default_driver(data_dir: DataDir) -> Box<dyn StorageDriver> {
    let choice = std::env::var("MVM_STORAGE_DRIVER").unwrap_or_default();
    match choice.as_str() {
        "copy" => return Box::new(CopyDriver::new(data_dir)),
        "overlay" => return Box::new(OverlayDriver::new(data_dir)),
        _ => {}
    }
    if is_root() && data_dir.ensure().is_ok() && OverlayDriver::probe(&data_dir) {
        Box::new(OverlayDriver::new(data_dir))
    } else {
        Box::new(CopyDriver::new(data_dir))
    }
}

pub(crate) fn storage_err(msg: impl Into<String>) -> Error {
    Error::Storage(msg.into())
}

pub(crate) fn is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}
