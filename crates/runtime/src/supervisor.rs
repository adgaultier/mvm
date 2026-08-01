//! Spawns and supervises shim processes from the daemon.

use mvm_common::Result;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::shim::ShimConfig;

/// Handle to a running shim process plus its captured console output.
pub struct ShimHandle {
    pub child: Child,
    /// Combined stdout+stderr of the shim (= guest console).
    pub console: std::process::ChildStdout,
    /// Write end of the guest console (only with `attach_stdin`); dropping
    /// it delivers EOF to the guest console.
    pub console_stdin: Option<std::process::ChildStdin>,
    pub config_path: PathBuf,
}

/// Write the shim config into `sandbox_dir` and spawn the shim as a
/// detached process (own session, console piped). With `attach_stdin`,
/// the shim's stdin is a pipe feeding the guest console; otherwise it is
/// /dev/null so console reads see EOF immediately.
pub fn spawn_shim(config: &ShimConfig, sandbox_dir: &Path, attach_stdin: bool) -> Result<ShimHandle> {
    let config_path = sandbox_dir.join("shim.json");
    config.save(&config_path)?;

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__vm-shim")
        .arg(&config_path)
        .stdin(if attach_stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Put the shim in its own session/process group so we can signal the
    // whole VM tree and it never receives our terminal's signals.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn()?;
    let console = child.stdout.take().expect("stdout piped");
    let console_stdin = child.stdin.take();

    // Redirect shim stderr into stdout's stream is not directly possible;
    // drain stderr in a thread that echoes into tracing.
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
