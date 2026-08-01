//! Rootless user-namespace mode for the daemon (podman-style).
//!
//! `mvm serve` re-execs itself inside a user namespace where uid 0 maps to
//! the invoking user and uids 1..N map to the user's /etc/subuid range.
//! Everything downstream — image unpack, storage drivers, and libkrun's
//! in-process virtiofs server — then runs as namespace-root with CAP_CHOWN
//! over the mapped range, so guest chown works *through virtiofs*.
//!
//! Requirements: /etc/subuid + /etc/subgid entries for the user and the
//! privileged `newuidmap`/`newgidmap` helpers. Without them the daemon runs
//! as before (guest chown limited to host credentials).
//!
//! Opt out with MVM_USERNS=0.

use std::os::unix::process::CommandExt;
use std::process::Command;

const CHILD_MARK: &str = "MVM_USERNS_CHILD";
const PIPE_FD: &str = "MVM_USERNS_PIPE";

/// Called by `serve` before the tokio runtime starts (the process must
/// still be single-threaded: unshare(CLONE_NEWUSER) fails otherwise).
/// Either returns quickly (child / disabled / unavailable) or re-execs and
/// never returns from the parent side.
pub fn maybe_enter_userns() {
    if std::env::var_os(CHILD_MARK).is_some() {
        finish_child();
        return;
    }
    if unsafe { libc::geteuid() } == 0 {
        return; // real root: nothing to gain
    }
    if matches!(
        std::env::var(MVM_USERNS).as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    ) {
        eprintln!("mvm: userns mode disabled (MVM_USERNS=0); guest chown will be limited");
        return;
    }

    let Some((sub_uid, sub_gid)) = subid_ranges() else {
        eprintln!(
            "mvm: no /etc/subuid+/etc/subgid entry (or newuidmap/newgidmap missing); \
             running without a user namespace — guest chown will be limited"
        );
        return;
    };

    // Pin the data dir before entering the namespace: inside it, euid is 0
    // and DataDir::resolve would wrongly pick /var/lib/mvm.
    if std::env::var_os("MVM_DATA_DIR").is_none() {
        if let Ok(d) = mvm_common::DataDir::resolve() {
            std::env::set_var("MVM_DATA_DIR", d.root());
        }
    }

    // Handshake pipe: the child blocks until the parent has written the
    // uid/gid maps (fds from pipe(2) are inherited across exec).
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!("mvm: pipe failed; running without a user namespace");
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("mvm: current_exe: {e}; running without a user namespace");
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1))
        .env(CHILD_MARK, "1")
        .env(PIPE_FD, fds[0].to_string());
    unsafe {
        cmd.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // If the parent dies, take the daemon down with it.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mvm: userns unshare failed ({e}); running without a user namespace");
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return;
        }
    };

    let pid = child.id();
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mapped = write_maps(pid, uid, gid, &sub_uid, &sub_gid);
    if !mapped {
        eprintln!("mvm: newuidmap/newgidmap failed; running without a user namespace");
        let _ = child.kill();
        let _ = child.wait();
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return;
    }

    // Release the child and become a thin supervisor.
    unsafe {
        libc::write(fds[1], [1u8].as_ptr() as *const libc::c_void, 1);
        libc::close(fds[1]);
        libc::close(fds[0]);
    }
    eprintln!(
        "mvm: userns mode active (uid 0 -> {uid}, 1.. -> {}+{})",
        sub_uid.start, sub_uid.count
    );
    let code = match child.wait() {
        Ok(status) => status
            .code()
            .unwrap_or_else(|| 128 + status_signal(&status)),
        Err(_) => 1,
    };
    std::process::exit(code);
}

const MVM_USERNS: &str = "MVM_USERNS";

fn status_signal(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.signal().unwrap_or(1)
}

/// Child side: wait for the parent to install the id maps, then finish
/// namespace setup (private mount namespace for the storage drivers).
fn finish_child() {
    if let Ok(fd) = std::env::var(PIPE_FD).and_then(|v| {
        v.parse::<i32>()
            .map_err(|_| std::env::VarError::NotPresent)
    }) {
        let mut byte = [0u8; 1];
        unsafe {
            libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1);
            libc::close(fd);
        }
    }
    std::env::remove_var(CHILD_MARK);
    std::env::remove_var(PIPE_FD);

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("mvm: warning: uid map not installed; userns setup incomplete");
        return;
    }

    // A private mount namespace so overlay mounts don't leak to the host.
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) == 0 {
            let root = std::ffi::CString::new("/").unwrap();
            libc::mount(
                std::ptr::null(),
                root.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            );
        }
    }
}

#[derive(Debug)]
pub struct SubidRange {
    pub start: u32,
    pub count: u32,
}

/// Find this user's subuid+subgid ranges; None when unavailable or when the
/// newuidmap/newgidmap helpers are missing.
fn subid_ranges() -> Option<(SubidRange, SubidRange)> {
    which("newuidmap")?;
    which("newgidmap")?;
    let uid = unsafe { libc::getuid() };
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    let sub_uid = parse_subid(&std::fs::read_to_string("/etc/subuid").ok()?, &user, uid)?;
    let sub_gid = parse_subid(&std::fs::read_to_string("/etc/subgid").ok()?, &user, uid)?;
    Some((sub_uid, sub_gid))
}

/// Parse an /etc/subuid-style file; entries may key on name or numeric id.
fn parse_subid(content: &str, user: &str, uid: u32) -> Option<SubidRange> {
    for line in content.lines() {
        let mut parts = line.trim().splitn(3, ':');
        let key = parts.next()?;
        if key != user && key != uid.to_string() {
            continue;
        }
        let start = parts.next()?.parse().ok()?;
        let count = parts.next()?.parse().ok()?;
        return Some(SubidRange { start, count });
    }
    None
}

fn write_maps(pid: u32, uid: u32, gid: u32, sub_uid: &SubidRange, sub_gid: &SubidRange) -> bool {
    // uid 0 -> the invoking user, 1..count -> the subordinate range.
    let uid_ok = Command::new("newuidmap")
        .args([
            pid.to_string(),
            "0".into(),
            uid.to_string(),
            "1".into(),
            "1".into(),
            sub_uid.start.to_string(),
            sub_uid.count.to_string(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let gid_ok = uid_ok
        && Command::new("newgidmap")
            .args([
                pid.to_string(),
                "0".into(),
                gid.to_string(),
                "1".into(),
                "1".into(),
                sub_gid.start.to_string(),
                sub_gid.count.to_string(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    uid_ok && gid_ok
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subid_by_name_and_uid() {
        let content = "alice:100000:65536\n1000:200000:65536\n";
        let byname = parse_subid(content, "alice", 42).unwrap();
        assert_eq!((byname.start, byname.count), (100000, 65536));
        let byuid = parse_subid(content, "bob", 1000).unwrap();
        assert_eq!((byuid.start, byuid.count), (200000, 65536));
        assert!(parse_subid(content, "carol", 7).is_none());
    }
}
