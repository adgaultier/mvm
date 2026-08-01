//! Spawns and supervises shim processes from the daemon.

use mvm_common::Result;
use std::fs::File;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::shim::ShimConfig;

/// Handle to a running shim process plus its captured console output.
pub struct ShimHandle {
    pub child: Child,
    /// Combined stdout+stderr of the shim (= guest console).
    pub console: File,
    /// Write end of the guest console (only with `attach_stdin`); dropping
    /// it delivers EOF to the guest console.
    pub console_stdin: Option<File>,
    pub config_path: PathBuf,
}

/// Write the shim config and spawn the shim with a pty-backed console. The
/// guest workload must see a terminal even when the client is not attached.
pub fn spawn_shim(config: &ShimConfig, sandbox_dir: &Path, attach_stdin: bool) -> Result<ShimHandle> {
    let config_path = sandbox_dir.join("shim.json");
    config.save(&config_path)?;

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    let (master, slave) = openpty()?;
    let stdin_fd = dup_fd(slave)?;
    let stdout_fd = dup_fd(slave)?;
    unsafe { libc::close(slave) };

    cmd.arg("__vm-shim")
        .arg(&config_path)
        .stdin(unsafe { Stdio::from(File::from_raw_fd(stdin_fd)) })
        .stdout(unsafe { Stdio::from(File::from_raw_fd(stdout_fd)) })
        .stderr(Stdio::piped());

    // Put the shim in its own session/process group so we can signal the
    // whole VM tree and it never receives our terminal's signals.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn()?;
    let console = unsafe { File::from_raw_fd(master) };
    let console_stdin = if attach_stdin {
        Some(unsafe { File::from_raw_fd(dup_fd(master)?) })
    } else {
        None
    };

    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match err.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let line = String::from_utf8_lossy(&buf[..n]);
                        tracing::debug!(target: "mvm::shim", "{}", line.trim_end());
                    }
                }
            }
        });
    }

    Ok(ShimHandle {
        child,
        console,
        console_stdin,
        config_path,
    })
}

fn openpty() -> Result<(RawFd, RawFd)> {
    let mut master = -1;
    let mut slave = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if rc == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut term) == 0 {
            term.c_oflag &= !libc::ONLCR;
            libc::tcsetattr(slave, libc::TCSANOW, &term);
        }
    }
    Ok((master, slave))
}

fn dup_fd(fd: RawFd) -> Result<RawFd> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(duplicated)
}
