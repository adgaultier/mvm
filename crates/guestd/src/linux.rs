//! mvm-guestd: PID 1 inside the guest microVM.
//!
//! Boots the workload, connects to the host over vsock (port 1024), and
//! serves exec requests. Single-threaded poll() event loop; reaps zombies
//! as PID 1. Built statically (crt-static) so it runs in any rootfs.

use std::collections::{HashMap, VecDeque};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use mvm_common::protocol::{self, encode_frame, FrameDecoder, GuestdEvent, GuestdRequest};

use crate::identity::{apply_user, resolve_user, GuestUser};
use crate::network;
use crate::pty;

const HOST_CID: u32 = 2; // VMADDR_CID_HOST
const CHUNK: usize = 8192;

struct BootTiming {
    phases: Vec<mvm_common::protocol::BootPhase>,
    current: Option<(&'static str, std::time::Instant)>,
}

impl BootTiming {
    fn new() -> Self {
        Self { phases: Vec::new(), current: None }
    }

    fn mark(&mut self, name: &'static str) {
        let now = std::time::Instant::now();
        if let Some((previous, start)) = self.current.take() {
            self.phases.push(mvm_common::protocol::BootPhase {
                name: format!("guestd_{previous}"),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        self.current = Some((name, now));
    }

    fn finish(mut self) -> Vec<mvm_common::protocol::BootPhase> {
        if let Some((name, start)) = self.current.take() {
            self.phases.push(mvm_common::protocol::BootPhase {
                name: format!("guestd_{name}"),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        self.phases
    }
}

static SELFPIPE_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn signal_handler(sig: i32) {
    let fd = SELFPIPE_W.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        let byte = [sig as u8];
        unsafe {
            // Best-effort, async-signal-safe, nonblocking (pipe is O_NONBLOCK).
            libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

struct Session {
    pid: i32,
    /// Pty session: stdin_fd and stdout_fd are the same fd (the pty
    /// master), stderr is merged into it, and it must be closed only once.
    tty: bool,
    stdin_fd: Option<RawFd>,
    stdin_buf: VecDeque<u8>,
    stdout_fd: Option<RawFd>,
    stderr_fd: Option<RawFd>,
    exit_code: Option<i32>,
}

impl Session {
    fn fully_drained(&self) -> bool {
        self.stdout_fd.is_none() && self.stderr_fd.is_none()
    }
}

struct Guestd {
    vsock: RawFd,
    out_queue: VecDeque<u8>,
    decoder: FrameDecoder,
    sessions: HashMap<u32, Session>,
    workload_pid: i32,
    workload_code: Option<i32>,
    /// Identity the workload runs as; exec sessions default to it.
    workload_user: GuestUser,
    /// Bridges the workload's guest pty to the console (`-t` only). Joined
    /// once the workload exits, before the guestd reports it: the workload
    /// closing its pty slave doesn't mean this thread has finished draining
    /// buffered pty output to the console yet, and `process::exit` doesn't
    /// wait for other threads — without the join, a short-lived `-t`
    /// workload can race its own exit against its last bytes ever reaching
    /// the host.
    console_output: Option<std::thread::JoinHandle<()>>,
    /// Guestd-owned dup of the workload pty master (the bridge owns the
    /// original; the input bridge its clone). Kept so the host can resize
    /// the console (`GuestdRequest::ConsoleResize`) without coupling to the
    /// bridge threads' lifetimes. Closed exactly once when the guestd drops.
    console_pty: Option<OwnedFd>,
    /// VM-scoped bearer token for the host's Agent API (vsock, see
    /// `mvm_common::protocol::AGENT_API_VSOCK_PORT`). Not yet used here, but
    /// captured and held so the mvm-agent-mcp bridge can present it.
    /// Scrubbed from the workload environment before it spawns.
    #[allow(dead_code)]
    guest_token: Option<String>,
    /// Strict security profile: exec sessions install the high-risk-syscall
    /// seccomp filter in their pre_exec, like the workload itself.
    strict: bool,
    /// Keeps the loaded program and its cgroup link alive for the VM lifetime.
    _ebpf: crate::ebpf::Installed,
}

pub fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let code = real_main(&argv[1..]);
    std::process::exit(code);
}
fn real_main(workload_argv: &[String]) -> i32 {
    let mut boot_timing = BootTiming::new();
    boot_timing.mark("seccomp");
    // 0. seccomp before anything else: the filter is inherited by every
    // process we spawn and can never be weakened, so the raw-socket ban
    // covers the workload, exec sessions and everything in between. It also
    // runs before any code that touches the rootfs or the network, so a
    // hostile workload cannot have prepared an environment that sidesteps it.
    // Failure to install is fatal — a guest that can't be sandboxed won't.
    if let Err(e) = crate::seccomp::install_raw_socket_filter() {
        eprintln!("mvm-guestd: seccomp: {e}");
        return 125;
    }

    boot_timing.mark("mounts");
    // 1. Mount any virtiofs bind mounts passed via MVM_MOUNTS.
    mount_bind_shares();

    // Static IPv4 bootstrap for NIC-backed modes (gvproxy defaults).
    boot_timing.mark("network");
    network::configure_network();

    // Install the bootstrap network hook before any untrusted workload exists.
    boot_timing.mark("ebpf");
    let ebpf = match crate::ebpf::install(network::dns_servers()) {
        Ok(ebpf) => ebpf,
        Err(e) => {
            eprintln!("mvm-guestd: eBPF bootstrap: {e}");
            return 125;
        }
    };

    // TSI mode: sockets are host-serviced (no NIC), but DNS still reads
    // /etc/resolv.conf, which most images ship empty.
    if std::env::var_os("MVM_NET_TSI").is_some() {
        let _ = std::fs::create_dir_all("/etc");
        let _ = std::fs::write(
            "/etc/resolv.conf",
            "nameserver 1.1.1.1\nnameserver 8.8.8.8\n",
        );
    }

    let console_tty = std::env::var_os("MVM_CONSOLE_TTY").is_some();
    let console_size = std::env::var("MVM_CONSOLE_SIZE").ok().and_then(|s| {
        let (cols, rows) = s.split_once(',')?;
        Some((cols.parse::<u16>().ok()?, rows.parse::<u16>().ok()?))
    });

    let user_spec = std::env::var("MVM_USER").ok();

    boot_timing.mark("setup");

    // Which OS the *host* runs; only "macos" is ever set (Linux needs no
    // signal). The guestd itself always runs in a Linux guest.
    let host_os = std::env::var("MVM_HOST_OS").unwrap_or_default();

    // VM-scoped bearer token for the host's Agent API. Deliberately NOT
    // scrubbed: it must reach the workload's environment so the tools it
    // spawns (the mvm-agent-mcp bridge) can authenticate over vsock.
    let guest_token = std::env::var("MVM_GUEST_TOKEN").ok();

    // Strict security profile: the workload (and everything it spawns) gets
    // an extra seccomp filter denying high-risk syscalls. Read before the
    // scrub below, and the flag itself is scrubbed like the other plumbing.
    let strict = std::env::var_os("MVM_SECURITY_STRICT").is_some();

    // Guest hostname, before the workload spawns.
    if let Ok(hostname) = std::env::var("MVM_HOSTNAME") {
        if !hostname.is_empty() {
            apply_hostname(&hostname);
        }
    }

    // Internal plumbing vars must not leak into the workload environment.
    for var in [
        "MVM_MOUNTS",
        "MVM_NET_CONFIG",
        "MVM_NET_TSI",
        "MVM_CONSOLE_TTY",
        "MVM_CONSOLE_SIZE",
        "MVM_USER",
        "MVM_HOSTNAME",
        "MVM_HOST_OS",
        "MVM_SECURITY_STRICT",
    ] {
        std::env::remove_var(var);
    }

    // Pty support for exec -t needs /dev/pts (libkrun's init mounts /dev
    // but not devpts). Best effort; EBUSY when already mounted is fine.
    pty::ensure_devpts();

    // Keep console output byte-oriented. The host owns the terminal display
    // policy; ONLCR here would turn every workload newline into CRLF in logs.
    // When we bridge to a workload pty the console must be fully raw: its own
    // line discipline would otherwise hold input until a newline and echo it
    // on top of the pty's echo.
    if console_tty {
        pty::raw_console_termios();
    } else {
        pty::normalize_console_termios();
    }

    // 2. Resolve the identity the workload runs as (image USER or `-u`), now
    // that the rootfs — and its /etc/passwd — is in place. Exec sessions
    // inherit it, docker-style, unless they ask for someone else.
    boot_timing.mark("identity");
    let user = match user_spec.as_deref() {
        None | Some("") => GuestUser::root(),
        Some(spec) => match resolve_user(spec) {
            Ok(user) => user,
            Err(e) => {
                eprintln!("mvm-guestd: {e}");
                return 125;
            }
        },
    };

    // On macOS the rootfs is copied by the host user, so files the image
    // declared as owned by `user` are actually owned by the host uid, and the
    // workload can't write its own home. Repair it before spawning. This flag
    // only reaches the guest on macOS hosts; Linux userns mode already
    // preserves ownership, and the uid check below makes it a no-op there.
    boot_timing.mark("ownership");
    if host_os == "macos" && !user.is_root() && !user.home.is_empty() && user.home != "/" {
        if let Ok(meta) = std::fs::symlink_metadata(&user.home) {
            if meta.uid() != user.uid || meta.gid() != user.gid {
                chown_tree(std::path::Path::new(&user.home), user.uid, user.gid);
            }
        }
    }

    // 3. Spawn the workload. The guest console is a byte stream, so an
    // interactive workload needs a real guest PTY of its own.
    let mut console_output = None;
    let mut console_pty = None;
    boot_timing.mark("workload");
    let workload = if workload_argv.is_empty() {
        None
    } else if console_tty {
        match pty::spawn_tty_workload(
            workload_argv,
            console_size,
            &user,
            strict,
        ) {
            Some((child, handle, pty)) => {
                console_output = Some(handle);
                console_pty = pty;
                Some(child)
            }
            None => return 127,
        }
    } else {
        let mut cmd = Command::new(&workload_argv[0]);
        cmd.args(&workload_argv[1..])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        apply_bpf_seccomp(&mut cmd);
        if strict {
            apply_strict_seccomp(&mut cmd);
        }
        apply_user(&mut cmd, &user);
        match cmd.spawn() {
            Ok(child) => Some(child),
            Err(e) => {
                eprintln!("mvm-guestd: failed to spawn {:?}: {e}", workload_argv[0]);
                return 127;
            }
        }
    };
    let workload_pid = workload.map(|c| c.id() as i32).unwrap_or(-1);

    // 4. Self-pipe for signals (must exist before handlers install).
    let mut pipe_fds = [-1i32; 2];
    unsafe {
        if libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) != 0 {
            // Without a self-pipe we can't multiplex signals; bail.
            return 1;
        }
    }
    let selfpipe_r = pipe_fds[0];
    SELFPIPE_W.store(pipe_fds[1], std::sync::atomic::Ordering::Relaxed);
    install_handlers();

    // 5. Connect to the host over vsock (retry while host listener comes up).
    boot_timing.mark("control_connect");
    let vsock = match connect_vsock_retry(HOST_CID, protocol::GUESTD_VSOCK_PORT, 100) {
        Some(fd) => fd,
        None => {
            // No control channel: still run the workload to completion.
            let mut status = 0;
            unsafe {
                libc::waitpid(workload_pid, &mut status, 0);
            }
            if let Some(handle) = console_output {
                let _ = handle.join();
            }
            return exit_status_to_code(status);
        }
    };

    let mut guestd = Guestd {
        vsock,
        out_queue: VecDeque::new(),
        decoder: FrameDecoder::default(),
        sessions: HashMap::new(),
        workload_pid,
        workload_code: None,
        workload_user: user,
        console_output,
        console_pty,
        guest_token,
        strict,
        _ebpf: ebpf,
    };
    let boot_phases = boot_timing.finish();
    guestd.send(&GuestdEvent::Ready {
        workload_pid: workload_pid.max(0) as u32,
        boot_phases,
    });

    guestd.run(selfpipe_r)
}

/// Apply no-new-privileges and drop CAP_CHOWN in a child before exec.
/// Registered *before* `apply_user` (whose pre_exec drops privileges). Failure
/// aborts the spawn. No logging inside the closure: it runs post-fork in the
/// child, where std locks may deadlock.
pub(crate) fn apply_strict_seccomp(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            set_no_new_privs().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("strict no-new-privileges failed: {error}"),
                )
            })?;
            drop_cap_chown().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("strict cap-drop failed: {error}"),
                )
            })?;
            crate::seccomp::install_strict_filter().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("strict seccomp install failed: {error}"),
                )
            })
        });
    }
}

pub(crate) fn apply_bpf_seccomp(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            crate::seccomp::install_bpf_filter().map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("BPF seccomp install failed: {error}"),
                )
            })
        });
    }
}

const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;

/// `CAP_CHOWN` = 0; not exposed by the pinned `libc` version.
const CAP_CHOWN: libc::c_int = 0;

fn set_no_new_privs() -> std::io::Result<()> {
    unsafe {
        if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Drop CAP_CHOWN from the process by removing it from the bounding set.
/// Dropping it from the bounding set also clears it from the inspired,
/// permitted and effective sets, and it cannot reappear via exec, setuid or
/// file capabilities. This stops a strict-mode workload (even a root one) from
/// re-owning live host data on `:v` (LinuxComplete) mounts — the mechanism
/// that prevents ownership divergence between nested parent/child workspaces.
/// Runs while still root (before `apply_user`), so the bounding set is full
/// and the drop succeeds.
fn drop_cap_chown() -> std::io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_CAPBSET_DROP, CAP_CHOWN, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

impl Guestd {
    fn send(&mut self, event: &GuestdEvent) {
        if let Ok(frame) = encode_frame(event) {
            self.out_queue.extend(frame);
            let _ = self.flush_out();
        }
    }

    fn flush_out(&mut self) -> std::io::Result<()> {
        while !self.out_queue.is_empty() {
            let n = unsafe {
                libc::write(
                    self.vsock,
                    self.out_queue.as_slices().0.as_ptr() as *const libc::c_void,
                    self.out_queue.len(),
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                return Err(err);
            }
            self.out_queue.drain(..n as usize);
        }
        Ok(())
    }

    fn run(&mut self, selfpipe_r: RawFd) -> i32 {
        let mut read_buf = [0u8; CHUNK];
        loop {
            // Build the pollfd set.
            let mut fds: Vec<libc::pollfd> = Vec::with_capacity(4 + self.sessions.len() * 3);
            let out_wanted = if self.out_queue.is_empty() {
                0
            } else {
                libc::POLLOUT
            };
            fds.push(libc::pollfd {
                fd: self.vsock,
                events: libc::POLLIN | out_wanted,
                revents: 0,
            });
            fds.push(libc::pollfd {
                fd: selfpipe_r,
                events: libc::POLLIN,
                revents: 0,
            });
            // Track which session/fd each pollfd belongs to.
            let mut fd_map: Vec<(u32, u8)> = Vec::new(); // (session id, 0=stdout,1=stderr,2=stdin)
            for (id, s) in self.sessions.iter() {
                if let Some(fd) = s.stdout_fd {
                    fds.push(libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                    fd_map.push((*id, 0));
                }
                if let Some(fd) = s.stderr_fd {
                    fds.push(libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                    fd_map.push((*id, 1));
                }
                if let Some(fd) = s.stdin_fd {
                    if !s.stdin_buf.is_empty() {
                        fds.push(libc::pollfd {
                            fd,
                            events: libc::POLLOUT,
                            revents: 0,
                        });
                        fd_map.push((*id, 2));
                    }
                }
            }

            let timeout = if self.workload_pid < 0 && self.sessions.is_empty() {
                60_000 // idle VM: wake up once a minute anyway
            } else {
                5_000
            };
            let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if n == 0 {
                // Opportunistic zombie reaping even without a signal.
                self.reap_children();
                continue;
            }

            // 1) vsock readable/writable
            let vsock_revents = fds[0].revents;
            if vsock_revents & libc::POLLOUT != 0 {
                let _ = self.flush_out();
            }
            if vsock_revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let n = unsafe {
                    libc::read(
                        self.vsock,
                        read_buf.as_mut_ptr() as *mut libc::c_void,
                        CHUNK,
                    )
                };
                if n <= 0 {
                    // Host gone: shut down.
                    self.kill_all();
                    return self.workload_code.unwrap_or(1);
                }
                match self.decoder.feed::<GuestdRequest>(&read_buf[..n as usize]) {
                    Ok(requests) => {
                        for req in requests {
                            self.handle_request(req);
                        }
                    }
                    Err(_) => {
                        self.send(&GuestdEvent::Error {
                            message: "corrupt frame from host".into(),
                        });
                    }
                }
            }

            // 2) signals
            if fds[1].revents & libc::POLLIN != 0 {
                let mut sigs = [0u8; 64];
                let n =
                    unsafe { libc::read(selfpipe_r, sigs.as_mut_ptr() as *mut libc::c_void, 64) };
                for &sig in &sigs[..n.max(0) as usize] {
                    self.handle_signal(sig as i32);
                }
            }

            // 3) session pipes
            let base = 2usize;
            for (i, (sid, kind)) in fd_map.iter().enumerate() {
                let revents = fds[base + i].revents;
                if revents == 0 {
                    continue;
                }
                let sid = *sid;
                match kind {
                    0 | 1 => self.pump_session_pipe(sid, *kind == 0, revents),
                    2 => self.flush_session_stdin(sid),
                    _ => {}
                }
            }

            // Sessions whose child exited and pipes are drained: report.
            let finished: Vec<u32> = self
                .sessions
                .iter()
                .filter(|(_, s)| s.exit_code.is_some() && s.fully_drained())
                .map(|(id, _)| *id)
                .collect();
            for sid in finished {
                let s = self.sessions.remove(&sid).unwrap();
                if let Some(fd) = s.stdin_fd {
                    unsafe { libc::close(fd) };
                }
                self.send(&GuestdEvent::Exit {
                    id: sid,
                    code: s.exit_code.unwrap_or(-1),
                });
            }

            // Workload finished and everything reported: we're done. Drain
            // the console-bridging thread first (see `console_output`) so a
            // short-lived `-t` workload's last bytes reach the host before
            // the process exits.
            if let Some(code) = self.workload_code {
                if let Some(handle) = self.console_output.take() {
                    let _ = handle.join();
                }
                self.send(&GuestdEvent::WorkloadExit { code });
                let _ = self.flush_out();
                self.kill_all_sessions();
                return code;
            }
        }
        1
    }

    fn handle_signal(&mut self, sig: i32) {
        match sig {
            libc::SIGCHLD => self.reap_children(),
            libc::SIGTERM | libc::SIGINT => {
                // Forward to workload + exec children.
                if self.workload_pid > 0 {
                    unsafe { libc::kill(self.workload_pid, sig) };
                }
                for s in self.sessions.values() {
                    unsafe { libc::kill(s.pid, sig) };
                }
                if self.workload_pid <= 0 {
                    self.workload_code = Some(128 + sig);
                }
            }
            _ => {}
        }
    }

    fn reap_children(&mut self) {
        loop {
            let mut status: i32 = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
            let code = exit_status_to_code(status);
            if pid == self.workload_pid {
                self.workload_pid = -1;
                self.workload_code = Some(code);
                continue;
            }
            if let Some((sid, _)) = self
                .sessions
                .iter()
                .find(|(_, s)| s.pid == pid)
                .map(|(a, b)| (*a, b.pid))
            {
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.exit_code = Some(code);
                    // Close stdin so readers see a clean EOF. Not for tty:
                    // stdin *is* the master, which the pump still drains.
                    if !s.tty {
                        if let Some(fd) = s.stdin_fd.take() {
                            unsafe { libc::close(fd) };
                        }
                    }
                }
            }
            // Unknown pids: reaped (zombie prevention as PID 1).
        }
    }

    fn pump_session_pipe(&mut self, sid: u32, is_stdout: bool, revents: i16) {
        let (fd_opt, sid_key) = {
            let s = match self.sessions.get(&sid) {
                Some(s) => s,
                None => return,
            };
            (if is_stdout { s.stdout_fd } else { s.stderr_fd }, sid)
        };
        let Some(fd) = fd_opt else { return };

        let mut buf = [0u8; CHUNK];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, CHUNK) };
        if n > 0 {
            let data = buf[..n as usize].to_vec();
            let event = if is_stdout {
                GuestdEvent::Stdout { id: sid_key, data }
            } else {
                GuestdEvent::Stderr { id: sid_key, data }
            };
            self.send(&event);
        } else if n == 0 || (revents & (libc::POLLHUP | libc::POLLERR) != 0) {
            unsafe { libc::close(fd) };
            if let Some(s) = self.sessions.get_mut(&sid) {
                if is_stdout {
                    s.stdout_fd = None;
                } else {
                    s.stderr_fd = None;
                }
                if s.tty {
                    // Same fd; it's gone now, don't close it a second time.
                    s.stdin_fd = None;
                }
            }
        }
    }

    fn flush_session_stdin(&mut self, sid: u32) {
        let Some(s) = self.sessions.get_mut(&sid) else {
            return;
        };
        let Some(fd) = s.stdin_fd else { return };
        while !s.stdin_buf.is_empty() {
            let n = unsafe {
                libc::write(
                    fd,
                    s.stdin_buf.as_slices().0.as_ptr() as *const libc::c_void,
                    s.stdin_buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            s.stdin_buf.drain(..n as usize);
        }
    }

    fn handle_request(&mut self, req: GuestdRequest) {
        match req {
            GuestdRequest::Ping => self.send(&GuestdEvent::Pong),
            GuestdRequest::Exec {
                id,
                argv,
                env,
                workdir,
                tty,
                cols,
                rows,
                user,
            } => self.spawn_exec(id, argv, env, workdir, tty, cols, rows, user),
            GuestdRequest::Stdin { id, data } => {
                if let Some(s) = self.sessions.get_mut(&id) {
                    s.stdin_buf.extend(data);
                    self.flush_session_stdin(id);
                }
            }
            GuestdRequest::StdinEof { id } => {
                if let Some(s) = self.sessions.get_mut(&id) {
                    if s.tty {
                        // A pty can't be half-closed; signal EOF the tty way.
                        s.stdin_buf.push_back(0x04); // VEOF (^D)
                        self.flush_session_stdin(id);
                    } else if let Some(fd) = s.stdin_fd.take() {
                        unsafe { libc::close(fd) };
                    }
                }
            }
            GuestdRequest::Resize { id, cols, rows } => {
                if let Some(s) = self.sessions.get(&id) {
                    if let (true, Some(fd)) = (s.tty, s.stdout_fd) {
                        let ws = libc::winsize {
                            ws_row: rows,
                            ws_col: cols,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
                    }
                }
            }
            GuestdRequest::ConsoleResize { cols, rows } => {
                if let Some(pty) = &self.console_pty {
                    let ws = libc::winsize {
                        ws_row: rows,
                        ws_col: cols,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    // The workload may already be gone (a resize that raced
                    // teardown); log, don't tear down the console.
                    if unsafe { libc::ioctl(pty.as_raw_fd(), libc::TIOCSWINSZ, &ws) } < 0 {
                        eprintln!(
                            "mvm-guestd: console resize {}x{} failed: {}",
                            cols,
                            rows,
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
            GuestdRequest::Kill { id } => {
                if let Some(s) = self.sessions.get(&id) {
                    unsafe { libc::kill(s.pid, libc::SIGKILL) };
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_exec(
        &mut self,
        id: u32,
        argv: Vec<String>,
        env: Vec<String>,
        workdir: Option<String>,
        tty: bool,
        cols: u16,
        rows: u16,
        user: Option<String>,
    ) {
        if argv.is_empty() {
            self.send(&GuestdEvent::Exit { id, code: 126 });
            return;
        }

        // Default to the workload's identity, like `docker exec`.
        let user = match user.as_deref() {
            None => self.workload_user.clone(),
            Some("") => GuestUser::root(),
            Some(spec) => match resolve_user(spec) {
                Ok(user) => user,
                Err(e) => {
                    self.send(&GuestdEvent::Error { message: e });
                    self.send(&GuestdEvent::Exit { id, code: 126 });
                    return;
                }
            },
        };

        // For tty sessions, allocate the pty up front; the child gets the
        // slave as its stdio and becomes the session leader on it.
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        if tty {
            let mut ws = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let wsp = if cols > 0 && rows > 0 {
                &mut ws as *mut libc::winsize
            } else {
                std::ptr::null_mut()
            };
            let rc = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    wsp,
                )
            };
            if rc != 0 {
                self.send(&GuestdEvent::Stderr {
                    id,
                    data: format!(
                        "mvm-guestd: openpty failed: {}\n",
                        std::io::Error::last_os_error()
                    )
                    .into_bytes(),
                });
                self.send(&GuestdEvent::Exit { id, code: 126 });
                return;
            }
            unsafe {
                libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).env_clear().envs(baseline_env());
        if tty {
            let stdio = |fd: RawFd| unsafe {
                use std::os::unix::io::FromRawFd;
                Stdio::from_raw_fd(libc::dup(fd))
            };
            cmd.stdin(stdio(slave))
                .stdout(stdio(slave))
                .stderr(stdio(slave));
            if !env.iter().any(|kv| kv.starts_with("TERM=")) {
                cmd.env("TERM", "xterm");
            }
            // Hand the terminal to whoever will own it after the drop.
            if !user.is_root() {
                unsafe { libc::fchown(slave, user.uid, user.gid) };
            }
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    // Make the pty the controlling terminal (fd 0 = slave).
                    libc::ioctl(0, libc::TIOCSCTTY, 0);
                    Ok(())
                });
            }
        } else {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
        for kv in env {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        // Strict mode: seccomp before the tty/pre_exec privilege work, so the
        // filter is active for the whole chain and for every process the
        // session spawns.
        apply_bpf_seccomp(&mut cmd);
        if self.strict {
            apply_strict_seccomp(&mut cmd);
        }
        // Last, so the tty work above still happens as root. Also overrides
        // HOME/USER/LOGNAME from baseline_env for the target identity.
        apply_user(&mut cmd, &user);
        let spawned = cmd.spawn();
        if tty {
            unsafe { libc::close(slave) };
        }
        match spawned {
            Ok(mut child) => {
                let pid = child.id() as i32;
                let (stdin_fd, stdout_fd, stderr_fd) = if tty {
                    set_nonblocking(master);
                    (Some(master), Some(master), None)
                } else {
                    let sin = child.stdin.take().map(|s| s.into_raw_fd());
                    let sout = child.stdout.take().map(|s| s.into_raw_fd());
                    let serr = child.stderr.take().map(|s| s.into_raw_fd());
                    for fd in [sin, sout, serr].into_iter().flatten() {
                        set_nonblocking(fd);
                    }
                    (sin, sout, serr)
                };
                // Detach the std Child so nobody waits on it but us.
                std::mem::forget(child);
                self.sessions.insert(
                    id,
                    Session {
                        pid,
                        tty,
                        stdin_fd,
                        stdin_buf: VecDeque::new(),
                        stdout_fd,
                        stderr_fd,
                        exit_code: None,
                    },
                );
            }
            Err(e) => {
                if tty {
                    unsafe { libc::close(master) };
                }
                self.send(&GuestdEvent::Stderr {
                    id,
                    data: format!("mvm-guestd: exec {:?} failed: {e}\n", argv[0]).into_bytes(),
                });
                self.send(&GuestdEvent::Exit { id, code: 127 });
            }
        }
    }

    fn kill_all_sessions(&mut self) {
        for s in self.sessions.values() {
            unsafe { libc::kill(s.pid, libc::SIGKILL) };
        }
        self.sessions.clear();
    }

    fn kill_all(&mut self) {
        if self.workload_pid > 0 {
            unsafe { libc::kill(self.workload_pid, libc::SIGKILL) };
        }
        self.kill_all_sessions();
    }
}

/// Recursively make `root` (and everything under it) owned by uid:gid.
/// Descriptor-relative traversal avoids rebuilding a full path for every
/// entry. `AT_SYMLINK_NOFOLLOW` keeps links owned but never traversed.
fn chown_tree(root: &std::path::Path, uid: u32, gid: u32) {
    let Ok(path) = std::ffi::CString::new(root.as_os_str().as_bytes()) else {
        return;
    };
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return;
    }
    chown_dir(fd, uid, gid);
}

fn chown_dir(fd: libc::c_int, uid: u32, gid: u32) {
    unsafe {
        let _ = libc::fchown(fd, uid, gid);
        let dir = libc::fdopendir(fd);
        if dir.is_null() {
            libc::close(fd);
            return;
        }
        let dir_fd = libc::dirfd(dir);
        loop {
            let entry = libc::readdir(dir);
            if entry.is_null() {
                break;
            }
            let entry = &*entry;
            let name = entry.d_name.as_ptr();
            if *name == 0 || (*name == b'.' as libc::c_char && *name.add(1) == 0) {
                continue;
            }
            if *name == b'.' as libc::c_char
                && *name.add(1) == b'.' as libc::c_char
                && *name.add(2) == 0
            {
                continue;
            }
            let _ = libc::fchownat(dir_fd, name, uid, gid, libc::AT_SYMLINK_NOFOLLOW);
            let is_dir = if entry.d_type == libc::DT_DIR {
                true
            } else if entry.d_type == libc::DT_UNKNOWN {
                let mut stat: libc::stat = std::mem::zeroed();
                libc::fstatat(dir_fd, name, &mut stat, libc::AT_SYMLINK_NOFOLLOW) == 0
                    && (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR
            } else {
                false
            };
            if is_dir {
                let child = libc::openat(
                    dir_fd,
                    name,
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                );
                if child >= 0 {
                    chown_dir(child, uid, gid);
                }
            }
        }
        libc::closedir(dir);
    }
}

fn baseline_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push((
            "PATH".into(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
        ));
    }
    env
}

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

fn install_handlers() {
    unsafe {
        let handler = signal_handler as *const () as libc::sighandler_t;
        libc::signal(libc::SIGCHLD, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
}

fn exit_status_to_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

/// Connect to the host's vsock listener, retrying until the shim has the
/// unix socket ready. Returns a nonblocking fd with CLOEXEC.
fn connect_vsock_retry(cid: u32, port: u32, attempts: u32) -> Option<RawFd> {
    #[repr(C)]
    struct SockaddrVm {
        svm_family: u16,
        svm_reserved1: u16,
        svm_port: u32,
        svm_cid: u32,
        svm_zero: [u8; 4],
    }
    const AF_VSOCK: i32 = 40;

    for _ in 0..attempts {
        let fd = unsafe { libc::socket(AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return None;
        }
        let addr = SockaddrVm {
            svm_family: AF_VSOCK as u16,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: cid,
            svm_zero: [0; 4],
        };
        let rc = unsafe {
            libc::connect(
                fd,
                &addr as *const SockaddrVm as *const libc::sockaddr,
                std::mem::size_of::<SockaddrVm>() as u32,
            )
        };
        if rc == 0 {
            set_nonblocking(fd);
            return Some(fd);
        }
        unsafe { libc::close(fd) };
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
}

/// Set the guest hostname: UTS name, /etc/hostname, and a 127.0.0.1
/// /etc/hosts entry (appended only if absent). Best-effort.
fn apply_hostname(hostname: &str) {
    let c_name = match std::ffi::CString::new(hostname) {
        Ok(c) => c,
        Err(_) => return,
    };
    if unsafe { libc::sethostname(c_name.as_ptr(), c_name.to_bytes().len()) } != 0 {
        eprintln!(
            "mvm-guestd: sethostname({hostname}): {}",
            std::io::Error::last_os_error()
        );
    }
    let _ = std::fs::write("/etc/hostname", format!("{hostname}\n"));

    let entry = format!("127.0.0.1 {hostname}");
    let present = std::fs::read_to_string("/etc/hosts")
        .map(|contents| contents.lines().any(|line| line.trim() == entry))
        .unwrap_or(false);
    if !present {
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open("/etc/hosts")
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{entry}")
            });
    }
}

/// Mount extra virtiofs shares listed in MVM_MOUNTS: "tag:guest[:ro];..."
fn mount_bind_shares() {
    let Ok(spec) = std::env::var("MVM_MOUNTS") else {
        return;
    };
    for entry in spec.split(';').filter(|e| !e.is_empty()) {
        let mut parts = entry.split(':');
        let (Some(tag), Some(guest)) = (parts.next(), parts.next()) else {
            continue;
        };
        let read_only = parts.next() == Some("ro");
        let _ = std::fs::create_dir_all(guest);
        let c_tag = match std::ffi::CString::new(tag) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let c_guest = match std::ffi::CString::new(guest) {
            Ok(g) => g,
            Err(_) => continue,
        };
        let c_type = std::ffi::CString::new("virtiofs").unwrap();
        let flags = if read_only { libc::MS_RDONLY } else { 0 };
        unsafe {
            libc::mount(
                c_tag.as_ptr(),
                c_guest.as_ptr(),
                c_type.as_ptr(),
                flags as libc::c_ulong,
                std::ptr::null(),
            );
        }
    }
}
