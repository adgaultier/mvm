//! Safe RAII wrapper over a libkrun configuration context.

use mvm_common::{Error, Result};
use std::ffi::{c_char, CString};
use std::path::Path;

use crate::check;

/// Owns a libkrun context. Freed on drop (unless `start_enter` diverges).
pub struct KrunContext {
    ctx: u32,
}

/// Helper keeping CString buffers alive while holding the raw pointer array.
struct CStringArray {
    #[allow(dead_code)]
    strings: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl CStringArray {
    fn new<I, S>(items: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut strings = Vec::new();
        let mut ptrs = Vec::new();
        for item in items {
            let s = CString::new(item.as_ref())
                .map_err(|_| Error::Runtime("string contains NUL byte".into()))?;
            ptrs.push(s.as_ptr());
            strings.push(s);
        }
        ptrs.push(std::ptr::null());
        Ok(Self { strings, ptrs })
    }
}

fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Runtime(format!("'{s}' contains a NUL byte")))
}

impl KrunContext {
    /// Create a new libkrun configuration context.
    pub fn new() -> Result<Self> {
        let rc = unsafe { krun_sys::krun_create_ctx() };
        if rc < 0 {
            return Err(Error::Runtime(format!(
                "krun_create_ctx failed: {} (is /dev/kvm accessible?)",
                std::io::Error::from_raw_os_error(-rc)
            )));
        }
        Ok(Self { ctx: rc as u32 })
    }

    /// Configure vCPU count and RAM (MiB).
    pub fn set_vm_config(&self, vcpus: u8, ram_mib: u32) -> Result<()> {
        check(
            unsafe { krun_sys::krun_set_vm_config(self.ctx, vcpus, ram_mib) },
            "krun_set_vm_config",
        )
    }

    /// Set a host directory as the guest root filesystem (virtiofs, rw).
    pub fn set_root(&self, path: &Path) -> Result<()> {
        let p = cstr(&path.to_string_lossy())?;
        check(
            unsafe { krun_sys::krun_set_root(self.ctx, p.as_ptr()) },
            "krun_set_root",
        )
    }

    /// Attach a raw disk image as a virtio-blk device (/dev/vd*).
    pub fn add_disk(&self, block_id: &str, path: &Path, read_only: bool) -> Result<()> {
        let b = cstr(block_id)?;
        let p = cstr(&path.to_string_lossy())?;
        check(
            unsafe { krun_sys::krun_add_disk(self.ctx, b.as_ptr(), p.as_ptr(), read_only) },
            "krun_add_disk",
        )
    }

    /// Add an extra virtio-fs mount (bind mount into the guest).
    pub fn add_virtiofs(&self, tag: &str, path: &Path, read_only: bool) -> Result<()> {
        let t = cstr(tag)?;
        let p = cstr(&path.to_string_lossy())?;
        check(
            unsafe { krun_sys::krun_add_virtiofs3(self.ctx, t.as_ptr(), p.as_ptr(), 0, read_only) },
            "krun_add_virtiofs3",
        )
    }

    /// Set the working directory for the workload.
    pub fn set_workdir(&self, dir: &str) -> Result<()> {
        let d = cstr(dir)?;
        check(
            unsafe { krun_sys::krun_set_workdir(self.ctx, d.as_ptr()) },
            "krun_set_workdir",
        )
    }

    /// Set the executable + argv + envp to run inside the guest.
    pub fn set_exec(&self, exec: &str, argv: &[String], envp: &[String]) -> Result<()> {
        let e = cstr(exec)?;
        let argv = CStringArray::new(argv.iter().map(|s| s.as_str()))?;
        let envp = CStringArray::new(envp.iter().map(|s| s.as_str()))?;
        check(
            unsafe {
                krun_sys::krun_set_exec(self.ctx, e.as_ptr(), argv.ptrs.as_ptr(), envp.ptrs.as_ptr())
            },
            "krun_set_exec",
        )
    }

    /// Map a guest vsock port to a host unix socket.
    /// `listen` = true when the host side initiates connections.
    pub fn add_vsock_port(&self, port: u32, socket_path: &Path, listen: bool) -> Result<()> {
        let p = cstr(&socket_path.to_string_lossy())?;
        check(
            unsafe { krun_sys::krun_add_vsock_port2(self.ctx, port, p.as_ptr(), listen) },
            "krun_add_vsock_port2",
        )
    }

    /// Attach a virtio-net device backed by a unixgram fd. Used with a
    /// dead socketpair end to *disable* libkrun's default TSI backend
    /// (transparent host networking) for truly isolated sandboxes.
    pub fn add_net_unixgram(&self, fd: std::os::unix::io::RawFd) -> Result<()> {
        let mac: [u8; 6] = [0x5a, 0x4d, 0x56, 0x4d, 0x00, 0x01]; // locally administered
        check(
            unsafe {
                krun_sys::krun_add_net_unixgram(
                    self.ctx,
                    std::ptr::null(),
                    fd,
                    mac.as_ptr(),
                    0,
                    0,
                )
            },
            "krun_add_net_unixgram",
        )
    }

    /// Attach to an existing TAP device.
    pub fn add_net_tap(&self, name: &str) -> Result<()> {
        let mut n = cstr(name)?.into_bytes_with_nul();
        check(
            unsafe {
                krun_sys::krun_add_net_tap(
                    self.ctx,
                    n.as_mut_ptr() as *mut c_char,
                    std::ptr::null(),
                    0,
                    0,
                )
            },
            "krun_add_net_tap",
        )
    }

    /// Use gvproxy userspace networking (NAT) via its socket.
    pub fn set_gvproxy(&self, socket: &Path) -> Result<()> {
        let mut p = cstr(&socket.to_string_lossy())?.into_bytes_with_nul();
        check(
            unsafe { krun_sys::krun_set_gvproxy_path(self.ctx, p.as_mut_ptr() as *mut c_char) },
            "krun_set_gvproxy_path",
        )
    }

    /// Set port mappings ("hostPort:guestPort") for userspace networking.
    pub fn set_port_map(&self, maps: &[String]) -> Result<()> {
        if maps.is_empty() {
            return Ok(());
        }
        let arr = CStringArray::new(maps.iter().map(|s| s.as_str()))?;
        check(
            unsafe { krun_sys::krun_set_port_map(self.ctx, arr.ptrs.as_ptr()) },
            "krun_set_port_map",
        )
    }

    /// Configure libkrun internal logging (global, process-wide).
    pub fn set_log_level(level: u32) {
        unsafe {
            krun_sys::krun_set_log_level(level);
        }
    }

    /// Enter the microVM. On success this never returns: the process exits
    /// with the guest workload's exit status. On failure returns an error.
    pub fn start_enter(self) -> Result<()> {
        let ctx = self.ctx;
        // Don't run Drop: start_enter consumes the context.
        std::mem::forget(self);
        let rc = unsafe { krun_sys::krun_start_enter(ctx) };
        check(rc, "krun_start_enter")
    }
}

impl Drop for KrunContext {
    fn drop(&mut self) {
        unsafe {
            krun_sys::krun_free_ctx(self.ctx);
        }
    }
}
