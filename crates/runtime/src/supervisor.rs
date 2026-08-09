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
pub fn spawn_shim(
    config: &ShimConfig,
    sandbox_dir: &Path,
    attach_stdin: bool,
) -> Result<ShimHandle> {
    let config_path = sandbox_dir.join("shim.json");
    config.save(&config_path)?;

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    let (master, slave) = openpty()?;
    // This pty is plumbing, not a terminal anyone types on: it exists so the
    // guest console is a tty. Its default line discipline (ICANON + ECHO)
    // would buffer host keystrokes until a newline, echo them a second time
    // on top of the guest's own echo, and turn ^C into a signal for the shim
    // instead of a byte for the guest. Raw = transparent conduit.
    make_raw(slave, config.console_size);
    let stdin_fd = dup_fd(slave)?;
    let stdout_fd = dup_fd(slave)?;
    // stderr joins the console instead of going somewhere only the daemon's
    // debug log could see. libkrun maps the guest's fd 2 to the shim's, so
    // piping it away silently discarded *all* guest stderr — a workload's
    // error output and the agent's own startup diagnostics both vanished,
    // leaving a bare exit code. Docker interleaves the two streams too.
    let stderr_fd = dup_fd(slave)?;
    unsafe { libc::close(slave) };

    cmd.arg("__vm-shim")
        .arg(&config_path)
        .stdin(unsafe { Stdio::from(File::from_raw_fd(stdin_fd)) })
        .stdout(unsafe { Stdio::from(File::from_raw_fd(stdout_fd)) })
        .stderr(unsafe { Stdio::from(File::from_raw_fd(stderr_fd)) });

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

    let child = cmd.spawn()?;
    let console = unsafe { File::from_raw_fd(master) };
    let console_stdin = if attach_stdin {
        Some(unsafe { File::from_raw_fd(dup_fd(master)?) })
    } else {
        None
    };

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
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((master, slave))
}

/// Raw termios (and, when known, a window size) on one end of a pty pair.
fn make_raw(fd: RawFd, size: Option<(u16, u16)>) {
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut term) == 0 {
            libc::cfmakeraw(&mut term);
            libc::tcsetattr(fd, libc::TCSANOW, &term);
        }
        if let Some((cols, rows)) = size {
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
        }
    }
}

fn dup_fd(fd: RawFd) -> Result<RawFd> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(duplicated)
}
