//! gvproxy: userspace NAT for `--net gvproxy`.
//!
//! A gvproxy vfkit *datagram* endpoint serves exactly one VM: gvproxy learns
//! the peer address from the first packet it receives on the unixgram socket
//! and keeps replying there for the rest of its life. It never re-learns, so a
//! second VM pointed at the same socket gets no traffic at all — and every mvm
//! guest boots on the same static address (192.168.127.2), so two VMs on one
//! gvproxy would collide anyway. Hence one gvproxy *per sandbox*, owned by the
//! daemon, unless the caller explicitly brought their own socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use mvm_common::{Error, Result};

const GUEST_IP: &str = "192.168.127.2";
/// How long to wait for a freshly spawned gvproxy to create its sockets.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A gvproxy process owned by the daemon on behalf of one sandbox.
pub struct Gvproxy {
    child: Child,
    /// vfkit datagram socket the guest's NIC talks to.
    pub vfkit: PathBuf,
    /// HTTP control socket used for port forwards.
    pub control: PathBuf,
}

impl Gvproxy {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Stop it and reap it. Dropping the handle would leave a zombie: it is
    /// our child, so somebody has to wait() for it. Blocking, but bounded.
    pub fn shutdown(mut self) {
        kill(self.child.id());
        for _ in 0..50 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The gvproxy binary to run (`MVM_GVPROXY_BIN` overrides the PATH lookup).
pub fn binary() -> PathBuf {
    std::env::var_os("MVM_GVPROXY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gvproxy"))
}

/// Start a private gvproxy for one sandbox, with both sockets in `dir`.
pub fn spawn(dir: &Path) -> Result<Gvproxy> {
    let vfkit = dir.join("gvproxy.sock");
    let control = dir.join("gvproxy-control.sock");
    // gvproxy refuses to bind a path that already exists.
    let _ = std::fs::remove_file(&vfkit);
    let _ = std::fs::remove_file(&control);

    let log = std::fs::File::create(dir.join("gvproxy.log"))?;
    let errlog = log.try_clone()?;
    let mut child = Command::new(binary())
        // -1 disables the SSH forward gvproxy would otherwise bind on 2222,
        // which would clash between sandboxes.
        .args(["-ssh-port", "-1"])
        .arg("-listen")
        .arg(format!("unix://{}", control.display()))
        .arg("-listen-vfkit")
        .arg(format!("unixgram://{}", vfkit.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errlog))
        .spawn()
        .map_err(|e| {
            Error::Network(format!(
                "cannot start {}: {e} (install gvproxy or set MVM_GVPROXY_BIN)",
                binary().display()
            ))
        })?;

    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        if vfkit.exists() && control.exists() {
            return Ok(Gvproxy { child, vfkit, control });
        }
        // A gvproxy that died on startup (unsupported unixgram scheme on old
        // builds, port clash, …) must not turn into a five-second stall.
        if let Ok(Some(status)) = child.try_wait() {
            let log = std::fs::read_to_string(dir.join("gvproxy.log")).unwrap_or_default();
            let detail = log.lines().last().unwrap_or("no output").to_string();
            return Err(Error::Network(format!(
                "gvproxy exited immediately ({status}): {detail}"
            )));
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Network(format!(
                "gvproxy did not create {} within {READY_TIMEOUT:?}",
                vfkit.display()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Terminate a gvproxy by pid (SIGTERM; it has no state worth draining).
/// For processes we no longer have a handle for — e.g. one left behind by a
/// previous daemon, which init will reap.
pub fn kill(pid: u32) {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
}

pub fn expose(control: &Path, ports: &[(u16, u16)]) -> Result<()> {
    for &(host, guest) in ports {
        request(
            control,
            "expose",
            format!(r#"{{"local":":{host}","remote":"{GUEST_IP}:{guest}"}}"#),
        )?;
    }
    Ok(())
}

pub fn unexpose(control: &Path, ports: &[(u16, u16)]) -> Result<()> {
    for &(host, _) in ports {
        request(control, "unexpose", format!(r#"{{"local":":{host}"}}"#))?;
    }
    Ok(())
}

fn request(control: &Path, endpoint: &str, body: String) -> Result<()> {
    let mut stream = UnixStream::connect(control).map_err(|e| {
        Error::Network(format!(
            "cannot connect to gvproxy control socket {}: {e}",
            control.display()
        ))
    })?;
    let request = format!(
        "POST /services/forwarder/{endpoint} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let status = String::from_utf8_lossy(&response);
    if !status.starts_with("HTTP/1.1 2") && !status.starts_with("HTTP/1.0 2") {
        let first_line = status.lines().next().unwrap_or("invalid response");
        return Err(Error::Network(format!(
            "gvproxy control request failed: {first_line}"
        )));
    }
    Ok(())
}
