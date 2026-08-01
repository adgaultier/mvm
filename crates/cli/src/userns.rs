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

/// Called by `serve` before the tokio runtime starts (the process must
/// still be single-threaded: unshare(CLONE_NEWUSER) fails otherwise).
/// Either returns quickly (child / disabled / unavailable) or re-execs and
/// never returns from the parent side.
pub fn maybe_enter_userns() {
    match std::env::var(CHILD_MARK).as_deref() {
        // Stage 1: we're inside the fresh user namespace, but we exec'd
        // before the id maps existed, so execve computed an *empty*
        // capability set (unmapped euid). Stop; the parent maps us and
        // SIGCONTs; then re-exec — this time as mapped uid 0, which grants
        // full capabilities in the namespace.
        Ok("1") => {
            unsafe { libc::raise(libc::SIGSTOP) };
            if unsafe { libc::geteuid() } != 0 {
                // Maps never landed; the parent handles fallback messaging.
                std::process::exit(1);
            }
            let exe = std::env::current_exe().unwrap_or_else(|_| "/proc/self/exe".into());
            let err = Command::new(exe)
                .args(std::env::args_os().skip(1))
                .env(CHILD_MARK, "2")
                .exec();
            eprintln!("mvm: userns re-exec failed: {err}");
            std::process::exit(1);
        }
        // Stage 2: mapped root with full caps; finish namespace setup.
        Ok(_) => {
            finish_child();
            return;
        }
        Err(_) => {}
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

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("mvm: current_exe: {e}; running without a user namespace");
            return;
        }
    };
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args_os().skip(1)).env(CHILD_MARK, "1");
    unsafe {
        cmd.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // If the parent dies, take the daemon down with it.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            // NOTE: the SIGSTOP handshake happens *after* exec (stage 1
            // above), not here — Command::spawn blocks until the child
            // execs, so stopping pre-exec would deadlock the parent.
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mvm: userns unshare failed ({e}); running without a user namespace");
            return;
        }
    };

    let pid = child.id();
    // Wait for the pre-exec SIGSTOP, install the maps, release the child.
    let mut status = 0i32;
    let stopped = unsafe { libc::waitpid(pid as i32, &mut status, libc::WUNTRACED) } == pid as i32
        && libc::WIFSTOPPED(status);
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let mapped = stopped && write_maps(pid, uid, gid, &sub_uid, &sub_gid);
    if !mapped {
        eprintln!("mvm: newuidmap/newgidmap failed; running without a user namespace");
        let _ = child.kill();
        unsafe { libc::kill(pid as i32, libc::SIGCONT) };
        let _ = child.wait();
        return;
    }
    unsafe { libc::kill(pid as i32, libc::SIGCONT) };
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

/// Child side. The parent installed the id maps while we were stopped
/// pre-exec, so we're already (mapped) root with full capabilities here;
/// finish namespace setup (private mount namespace for storage drivers).
fn finish_child() {
    std::env::remove_var(CHILD_MARK);

    if unsafe { libc::geteuid() } != 0 {
        eprintln!("mvm: warning: uid map not installed; userns setup incomplete");
        return;
    }

    // A private mount namespace so overlay mounts don't leak to the host.
    unsafe {
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            eprintln!(
                "mvm: warning: mount-namespace unshare failed: {} (overlay storage may not work)",
                std::io::Error::last_os_error()
            );
            return;
        }
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
