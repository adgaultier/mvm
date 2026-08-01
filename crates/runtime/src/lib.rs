//! Safe wrapper over libkrun plus the VM shim used to supervise microVMs.

pub mod shim;
pub mod supervisor;
pub mod vm;

pub use shim::{run_shim, ShimConfig};
pub use supervisor::{spawn_shim, ShimHandle};
pub use vm::KrunContext;

use mvm_common::{Error, Result};

/// Check a libkrun return code (negative = -errno).
pub(crate) fn check(rc: i32, what: &str) -> Result<()> {
    if rc < 0 {
        Err(Error::Runtime(format!(
            "{what} failed: {}",
            std::io::Error::from_raw_os_error(-rc)
        )))
    } else {
        Ok(())
    }
}
