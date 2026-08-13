//! mvm-agent: PID 1 inside the guest microVM.
//!
//! Boots the workload, connects to the host over vsock (port 1024), and
//! serves exec requests. Single-threaded poll() event loop; reaps zombies
//! as PID 1. Built statically (crt-static) so it runs in any rootfs.

use std::collections::{HashMap, VecDeque};
use std::os::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use mvm_common::protocol::{self, encode_frame, AgentEvent, AgentRequest, FrameDecoder};

const HOST_CID: u32 = 2; // VMADDR_CID_HOST
const CHUNK: usize = 8192;

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

struct Agent {
    vsock: RawFd,
    out_queue: VecDeque<u8>,
    decoder: FrameDecoder,
    sessions: HashMap<u32, Session>,
    workload_pid: i32,
    workload_code: Option<i32>,
    /// Identity the workload runs as; exec sessions default to it.
    workload_user: GuestUser,
    /// Bridges the workload's guest pty to the console (`-t` only). Joined
    /// once the workload exits, before the agent reports it: the workload
    /// closing its pty slave doesn't mean this thread has finished draining
    /// buffered pty output to the console yet, and `process::exit` doesn't
    /// wait for other threads — without the join, a short-lived `-t`
    /// workload can race its own exit against its last bytes ever reaching
    /// the host.
    console_output: Option<std::thread::JoinHandle<()>>,
    /// Agent-owned dup of the workload pty master (the bridge owns the
    /// original; the input bridge its clone). Kept so the host can resize
    /// the console (`AgentRequest::ConsoleResize`) without coupling to the
    /// bridge threads' lifetimes. Closed exactly once when the agent drops.
    console_pty: Option<OwnedFd>,
    /// VM-scoped bearer token for the host's Agent API (`/agent/v1`). Not
    /// yet used here (no in-guest HTTP client), but captured and held so the
    /// MCP bridge can present it when that lands. Scrubbbed from the workload
    /// environment before it spawns.
    #[allow(dead_code)]
    agent_token: Option<String>,
}

pub fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let code = real_main(&argv[1..]);
    std::process::exit(code);
}
fn real_main(workload_argv: &[String]) -> i32 {
    // 0. seccomp before anything else: the filter is inherited by every
    // process we spawn and can never be weakened, so the raw-socket ban
    // covers the workload, exec sessions and everything in between. It also
    // runs before any code that touches the rootfs or the network, so a
    // hostile workload cannot have prepared an environment that sidesteps it.
    // Failure to install is fatal — a guest that can't be sandboxed won't.
    if let Err(e) = crate::seccomp::install_raw_socket_filter() {
        eprintln!("mvm-agent: seccomp: {e}");
        return 125;
    }

    // 1. Mount any virtiofs bind mounts passed via MVM_MOUNTS.
    mount_bind_shares();

    // Static IPv4 bootstrap for NIC-backed modes (gvproxy defaults).
    configure_network();

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

    // VM-scoped bearer token for the host's Agent API. Captured before the
    // scrub: it belongs to the agent, never to the workload.
    let agent_token = std::env::var("MVM_AGENT_TOKEN").ok();

    // Internal plumbing vars must not leak into the workload environment.
    for var in [
        "MVM_MOUNTS",
        "MVM_NET_CONFIG",
        "MVM_NET_TSI",
        "MVM_CONSOLE_TTY",
        "MVM_CONSOLE_SIZE",
        "MVM_USER",
        "MVM_AGENT_TOKEN",
    ] {
        std::env::remove_var(var);
    }

    // Pty support for exec -t needs /dev/pts (libkrun's init mounts /dev
    // but not devpts). Best effort; EBUSY when already mounted is fine.
    ensure_devpts();

    // Keep console output byte-oriented. The host owns the terminal display
    // policy; ONLCR here would turn every workload newline into CRLF in logs.
    // When we bridge to a workload pty the console must be fully raw: its own
    // line discipline would otherwise hold input until a newline and echo it
    // on top of the pty's echo.
    if console_tty {
        raw_console_termios();
    } else {
        normalize_console_termios();
    }

    // 2. Resolve the identity the workload runs as (image USER or `-u`), now
    // that the rootfs — and its /etc/passwd — is in place. Exec sessions
    // inherit it, docker-style, unless they ask for someone else.
    let user = match user_spec.as_deref() {
        None | Some("") => GuestUser::root(),
        Some(spec) => match resolve_user(spec) {
            Ok(user) => user,
            Err(e) => {
                eprintln!("mvm-agent: {e}");
                return 125;
            }
        },
    };

    // 3. Spawn the workload. The guest console is a byte stream, so an
    // interactive workload needs a real guest PTY of its own.
    let mut console_output = None;
    let mut console_pty = None;
    let workload = if workload_argv.is_empty() {
        None
    } else if console_tty {
        match spawn_tty_workload(workload_argv, console_size, &user) {
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
        apply_user(&mut cmd, &user);
        match cmd.spawn() {
            Ok(child) => Some(child),
            Err(e) => {
                eprintln!("mvm-agent: failed to spawn {:?}: {e}", workload_argv[0]);
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
    let vsock = match connect_vsock_retry(HOST_CID, protocol::AGENT_VSOCK_PORT, 100) {
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

    let mut agent = Agent {
        vsock,
        out_queue: VecDeque::new(),
        decoder: FrameDecoder::default(),
        sessions: HashMap::new(),
        workload_pid,
        workload_code: None,
        workload_user: user,
        console_output,
        console_pty,
        agent_token,
    };
    agent.send(&AgentEvent::Ready {
        workload_pid: workload_pid.max(0) as u32,
    });

    agent.run(selfpipe_r)
}

fn spawn_tty_workload(
    workload_argv: &[String],
    size: Option<(u16, u16)>,
    user: &GuestUser,
) -> Option<(
    std::process::Child,
    std::thread::JoinHandle<()>,
    Option<OwnedFd>,
)> {
    let winsize = size.map(|(cols, rows)| libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    });
    let mut fds = [-1; 2];
    let rc = unsafe {
        libc::openpty(
            &mut fds[0],
            &mut fds[1],
            std::ptr::null_mut(),
            std::ptr::null(),
            winsize
                .as_ref()
                .map(|w| w as *const libc::winsize)
                .unwrap_or(std::ptr::null()),
        )
    };
    if rc != 0 {
        eprintln!(
            "mvm-agent: openpty failed: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    // Don't trust the guest kernel's pty defaults: this VM's fresh slaves come
    // up with ONLCR clear, which would send bare LFs to a client in raw mode
    // (every line starting where the last one ended). The workload's pty is
    // the only line discipline left in the chain, so spell out the terminal
    // behaviour it must provide.
    interactive_pty_termios(fds[1]);
    // The workload owns this terminal, so it must be able to reopen /dev/tty
    // and change its settings after dropping privileges.
    if !user.is_root() {
        unsafe { libc::fchown(fds[1], user.uid, user.gid) };
    }
    let master = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let slave = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    let slave_out = slave.try_clone().ok()?;
    let slave_err = slave.try_clone().ok()?;
    let mut cmd = Command::new(&workload_argv[0]);
    cmd.args(&workload_argv[1..])
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::from(slave_err));
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Registered last: pre_exec closures run in order, and claiming the
    // controlling terminal has to happen while still root.
    apply_user(&mut cmd, user);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("mvm-agent: failed to spawn {:?}: {e}", workload_argv[0]);
            return None;
        }
    };

    let input_fd = unsafe { libc::dup(0) };
    let output_fd = unsafe { libc::dup(1) };
    if input_fd < 0 || output_fd < 0 {
        // No bridging possible either way; hand back an already-finished
        // handle so the caller can still unconditionally join it.
        return Some((child, std::thread::spawn(|| {}), None));
    }
    let mut input = unsafe { std::fs::File::from_raw_fd(input_fd) };
    let Ok(mut input_master) = master.try_clone() else {
        return Some((child, std::thread::spawn(|| {}), None));
    };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut input_master);
    });
    let mut output = unsafe { std::fs::File::from_raw_fd(output_fd) };
    let mut output_master = master;
    // The agent's own handle to the workload pty. Dup'd off the master before
    // it moves into the output bridge thread: the bridge (and the input
    // bridge's clone) keep their existing ownership, this fd is independent
    // of both and closes when the agent drops.
    let console_pty = output_master.try_clone().ok().map(OwnedFd::from);
    let output_handle = std::thread::spawn(move || {
        let _ = std::io::copy(&mut output_master, &mut output);
    });
    Some((child, output_handle, console_pty))
}

/// A resolved guest identity: what the image's `USER` (or `-u`) means once
/// looked up in *this* rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestUser {
    uid: u32,
    gid: u32,
    /// Primary gid first, then supplementary groups from /etc/group.
    groups: Vec<u32>,
    /// Name for USER/LOGNAME; the numeric uid when passwd has no entry.
    name: String,
    home: String,
}

impl GuestUser {
    fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            groups: vec![0],
            name: "root".into(),
            home: "/root".into(),
        }
    }

    fn is_root(&self) -> bool {
        self.uid == 0 && self.gid == 0
    }
}

/// Resolve a docker-style user spec — `name`, `uid`, `name:group`, `uid:gid` —
/// against the rootfs. Errors the way docker does when a name is unknown:
/// running as the wrong identity is worse than not running.
fn resolve_user(spec: &str) -> Result<GuestUser, String> {
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
    resolve_user_from(spec, &passwd, &group)
}

/// The lookup itself, over the file contents — pure, so it can be tested
/// against real image fixtures instead of whatever the host happens to have.
fn resolve_user_from(spec: &str, passwd: &str, group_file: &str) -> Result<GuestUser, String> {
    let (user_part, group_part) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };

    // passwd: name:passwd:uid:gid:gecos:home:shell
    let entry = passwd.lines().find(|line| {
        let mut f = line.split(':');
        match (f.next(), f.nth(1)) {
            (Some(name), Some(uid)) => name == user_part || uid == user_part,
            _ => false,
        }
    });
    let (name, uid, mut gid, home) = match entry {
        Some(line) => {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 6 {
                return Err(format!("malformed /etc/passwd entry for '{user_part}'"));
            }
            let uid: u32 = f[2]
                .parse()
                .map_err(|_| format!("bad uid in /etc/passwd for '{user_part}'"))?;
            let gid: u32 = f[3]
                .parse()
                .map_err(|_| format!("bad gid in /etc/passwd for '{user_part}'"))?;
            (f[0].to_string(), uid, gid, f[5].to_string())
        }
        // No entry: a numeric id is still usable (docker allows it), a name is not.
        None => match user_part.parse::<u32>() {
            Ok(uid) => (user_part.to_string(), uid, 0, "/".to_string()),
            Err(_) => {
                return Err(format!(
                    "unable to find user '{user_part}': no matching entry in /etc/passwd"
                ))
            }
        },
    };

    // group: name:passwd:gid:members
    let gid_of = |want: &str| -> Option<u32> {
        group_file.lines().find_map(|line| {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 3 && (f[0] == want || f[2] == want) {
                f[2].parse().ok()
            } else {
                None
            }
        })
    };
    if let Some(group_part) = group_part {
        gid = match gid_of(group_part) {
            Some(gid) => gid,
            None => group_part.parse::<u32>().map_err(|_| {
                format!("unable to find group '{group_part}': no matching entry in /etc/group")
            })?,
        };
    }

    // Supplementary groups: every group listing this user as a member.
    let mut groups = vec![gid];
    for line in group_file.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 4 {
            continue;
        }
        let Ok(g) = f[2].parse::<u32>() else { continue };
        if g != gid && f[3].split(',').any(|m| !m.is_empty() && m == name) {
            groups.push(g);
        }
    }

    Ok(GuestUser {
        uid,
        gid,
        groups,
        name,
        home,
    })
}

/// Run a child as `user`: identity env for the workload, and the actual
/// privilege drop between fork and exec.
fn apply_user(cmd: &mut Command, user: &GuestUser) {
    cmd.env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name);
    if user.is_root() {
        return;
    }
    // Everything the closure needs is allocated here, before the fork.
    let (uid, gid, groups) = (user.uid, user.gid, user.groups.clone());
    unsafe {
        cmd.pre_exec(move || {
            // Groups then gid then uid: dropping the uid first would forfeit
            // the privilege needed for the other two.
            if libc::setgroups(groups.len() as _, groups.as_ptr()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Sane interactive defaults on a pty slave: CR/LF translation both ways,
/// echo, line editing, and ^C/^Z signalling.
fn interactive_pty_termios(slave: RawFd) {
    unsafe {
        let mut term: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut term) != 0 {
            return;
        }
        term.c_iflag |= libc::ICRNL | libc::IXON | libc::BRKINT;
        term.c_oflag |= libc::OPOST | libc::ONLCR;
        term.c_lflag |= libc::ISIG
            | libc::ICANON
            | libc::ECHO
            | libc::ECHOE
            | libc::ECHOK
            | libc::ECHOCTL
            | libc::ECHOKE
            | libc::IEXTEN;
        let _ = libc::tcsetattr(slave, libc::TCSANOW, &term);
    }
}

/// Fully transparent console: no echo, no canonical buffering, no signal
/// generation — every byte belongs to the workload's own pty.
fn raw_console_termios() {
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(0, &mut term) == 0 {
            libc::cfmakeraw(&mut term);
            let _ = libc::tcsetattr(0, libc::TCSANOW, &term);
        }
    }
}

fn normalize_console_termios() {
    unsafe {
        let mut term = std::mem::zeroed();
        if libc::tcgetattr(0, &mut term) == 0 {
            term.c_oflag &= !libc::ONLCR;
            let _ = libc::tcsetattr(0, libc::TCSANOW, &term);
        }
    }
}

impl Agent {
    fn send(&mut self, event: &AgentEvent) {
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
                match self.decoder.feed::<AgentRequest>(&read_buf[..n as usize]) {
                    Ok(requests) => {
                        for req in requests {
                            self.handle_request(req);
                        }
                    }
                    Err(_) => {
                        self.send(&AgentEvent::Error {
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
                self.send(&AgentEvent::Exit {
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
                self.send(&AgentEvent::WorkloadExit { code });
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
                AgentEvent::Stdout { id: sid_key, data }
            } else {
                AgentEvent::Stderr { id: sid_key, data }
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

    fn handle_request(&mut self, req: AgentRequest) {
        match req {
            AgentRequest::Ping => self.send(&AgentEvent::Pong),
            AgentRequest::Exec {
                id,
                argv,
                env,
                workdir,
                tty,
                cols,
                rows,
                user,
            } => self.spawn_exec(id, argv, env, workdir, tty, cols, rows, user),
            AgentRequest::Stdin { id, data } => {
                if let Some(s) = self.sessions.get_mut(&id) {
                    s.stdin_buf.extend(data);
                    self.flush_session_stdin(id);
                }
            }
            AgentRequest::StdinEof { id } => {
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
            AgentRequest::Resize { id, cols, rows } => {
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
            AgentRequest::ConsoleResize { cols, rows } => {
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
                            "mvm-agent: console resize {}x{} failed: {}",
                            cols,
                            rows,
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
            AgentRequest::Kill { id } => {
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
            self.send(&AgentEvent::Exit { id, code: 126 });
            return;
        }

        // Default to the workload's identity, like `docker exec`.
        let user = match user.as_deref() {
            None => self.workload_user.clone(),
            Some("") => GuestUser::root(),
            Some(spec) => match resolve_user(spec) {
                Ok(user) => user,
                Err(e) => {
                    self.send(&AgentEvent::Error { message: e });
                    self.send(&AgentEvent::Exit { id, code: 126 });
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
                self.send(&AgentEvent::Stderr {
                    id,
                    data: format!(
                        "mvm-agent: openpty failed: {}\n",
                        std::io::Error::last_os_error()
                    )
                    .into_bytes(),
                });
                self.send(&AgentEvent::Exit { id, code: 126 });
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
                self.send(&AgentEvent::Stderr {
                    id,
                    data: format!("mvm-agent: exec {:?} failed: {e}\n", argv[0]).into_bytes(),
                });
                self.send(&AgentEvent::Exit { id, code: 127 });
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

/// Configure eth0 statically from MVM_NET_CONFIG="<ip>/<prefix>,<gateway>"
/// (gvproxy vfkit defaults: 192.168.127.2/24 via 192.168.127.1). The
/// gateway doubles as the DNS server. Skipped when eth0 already has an
/// address. Pure ioctls — works in any image, no `ip` binary required.
fn configure_network() {
    let Ok(spec) = std::env::var("MVM_NET_CONFIG") else {
        return;
    };
    let parsed = (|| {
        let (addr, gw) = spec.split_once(',')?;
        let (ip, prefix) = addr.split_once('/')?;
        let ip: std::net::Ipv4Addr = ip.parse().ok()?;
        let prefix: u32 = prefix.parse().ok()?;
        let gw: std::net::Ipv4Addr = gw.parse().ok()?;
        Some((ip, prefix.min(32), gw))
    })();
    let Some((ip, prefix, gw)) = parsed else {
        eprintln!("mvm-agent: bad MVM_NET_CONFIG '{spec}'");
        return;
    };

    unsafe {
        let sock = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if sock < 0 {
            return;
        }

        // Loopback up (usually already done by the kernel/init).
        set_iface_flags(sock, "lo");

        // Skip if eth0 already has an IPv4 address.
        let mut req = ifreq("eth0");
        if libc::ioctl(sock, libc::SIOCGIFADDR as _, &mut req) == 0 {
            let sin = &req.ifr_ifru.ifru_addr as *const libc::sockaddr as *const libc::sockaddr_in;
            if (*sin).sin_addr.s_addr != 0 {
                libc::close(sock);
                write_resolv_conf(gw);
                return;
            }
        }

        // Address + netmask.
        let mut req = ifreq("eth0");
        put_sockaddr_in(&mut req.ifr_ifru.ifru_addr, ip);
        if libc::ioctl(sock, libc::SIOCSIFADDR as _, &req) != 0 {
            eprintln!(
                "mvm-agent: SIOCSIFADDR: {}",
                std::io::Error::last_os_error()
            );
            libc::close(sock);
            return;
        }
        let mask = std::net::Ipv4Addr::from(u32::MAX.checked_shl(32 - prefix).unwrap_or(0));
        let mut req = ifreq("eth0");
        put_sockaddr_in(&mut req.ifr_ifru.ifru_netmask, mask);
        libc::ioctl(sock, libc::SIOCSIFNETMASK as _, &req);

        set_iface_flags(sock, "eth0");

        // Default route via the gateway.
        let mut route: libc::rtentry = std::mem::zeroed();
        put_sockaddr_in_raw(&mut route.rt_dst, std::net::Ipv4Addr::UNSPECIFIED);
        put_sockaddr_in_raw(&mut route.rt_genmask, std::net::Ipv4Addr::UNSPECIFIED);
        put_sockaddr_in_raw(&mut route.rt_gateway, gw);
        route.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;
        if libc::ioctl(sock, libc::SIOCADDRT as _, &route) != 0 {
            eprintln!("mvm-agent: SIOCADDRT: {}", std::io::Error::last_os_error());
        }
        libc::close(sock);
    }

    write_resolv_conf(gw);
}

fn write_resolv_conf(dns: std::net::Ipv4Addr) {
    let _ = std::fs::create_dir_all("/etc");
    let _ = std::fs::write("/etc/resolv.conf", format!("nameserver {dns}\n"));
}

unsafe fn set_iface_flags(sock: i32, name: &str) {
    let mut req = ifreq(name);
    if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut req) } == 0 {
        unsafe {
            req.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &req);
        }
    }
}

fn ifreq(name: &str) -> libc::ifreq {
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    for (i, b) in name.as_bytes().iter().take(libc::IFNAMSIZ - 1).enumerate() {
        req.ifr_name[i] = *b as libc::c_char;
    }
    req
}

fn put_sockaddr_in(slot: &mut libc::sockaddr, ip: std::net::Ipv4Addr) {
    put_sockaddr_in_raw(slot, ip);
}

fn put_sockaddr_in_raw(slot: &mut libc::sockaddr, ip: std::net::Ipv4Addr) {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(ip).to_be(),
        },
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::write(slot as *mut libc::sockaddr as *mut libc::sockaddr_in, sin);
    }
}

/// Mount devpts at /dev/pts so openpty works (pty slaves live there).
fn ensure_devpts() {
    let _ = std::fs::create_dir_all("/dev/pts");
    let c_src = std::ffi::CString::new("devpts").unwrap();
    let c_target = std::ffi::CString::new("/dev/pts").unwrap();
    let c_data = std::ffi::CString::new("mode=0620,ptmxmode=0666").unwrap();
    unsafe {
        libc::mount(
            c_src.as_ptr(),
            c_target.as_ptr(),
            c_src.as_ptr(), // fstype == "devpts" too
            0,
            c_data.as_ptr() as *const libc::c_void,
        );
    }

    // devpts creates guest PTYs only when /dev/ptmx points at this instance.
    // Some minimal roots provide a stale devtmpfs /dev/ptmx character node.
    let needs_ptmx_link = std::fs::symlink_metadata("/dev/ptmx")
        .map(|m| !m.file_type().is_symlink())
        .unwrap_or(true);
    if needs_ptmx_link {
        let _ = std::fs::remove_file("/dev/ptmx");
        let _ = std::os::unix::fs::symlink("/dev/pts/ptmx", "/dev/ptmx");
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

#[cfg(test)]
mod tests {
    use super::{resolve_user_from, GuestUser};

    // Trimmed from a real alpine image, plus a docker-style app user.
    const PASSWD: &str = "root:x:0:0:root:/root:/bin/ash\n\
                          bin:x:1:1:bin:/bin:/sbin/nologin\n\
                          agent:x:1000:1000:agent:/home/agent:/bin/sh\n\
                          nobody:x:65534:65534:nobody:/:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:root\n\
                         bin:x:1:root,bin,daemon\n\
                         agent:x:1000:\n\
                         docker:x:998:agent\n\
                         wheel:x:10:agent,bin\n\
                         nobody:x:65534:\n";

    fn resolve(spec: &str) -> GuestUser {
        resolve_user_from(spec, PASSWD, GROUP).expect("resolves")
    }

    #[test]
    fn resolves_by_name_with_home_and_groups() {
        let u = resolve("agent");
        assert_eq!((u.uid, u.gid), (1000, 1000));
        assert_eq!(u.name, "agent");
        assert_eq!(u.home, "/home/agent");
        // Primary gid first, then every group listing the user as a member.
        assert_eq!(u.groups, vec![1000, 998, 10]);
        assert!(!u.is_root());
    }

    #[test]
    fn resolves_by_uid_and_by_explicit_group() {
        assert_eq!(resolve("1000").name, "agent");
        assert_eq!(resolve("65534").home, "/");

        let u = resolve("agent:bin");
        assert_eq!((u.uid, u.gid), (1000, 1));
        // wheel/docker list "agent", bin is now primary, so it is not repeated.
        assert_eq!(u.groups, vec![1, 998, 10]);

        let u = resolve("agent:4242");
        assert_eq!(u.gid, 4242);
        assert_eq!(u.groups[0], 4242);
    }

    #[test]
    fn root_is_recognised_as_root() {
        let u = resolve("root");
        assert!(u.is_root());
        assert_eq!(u.home, "/root");
        assert!(GuestUser::root().is_root());
    }

    #[test]
    fn unknown_numeric_id_is_allowed_unknown_name_is_not() {
        // docker: a bare uid needs no passwd entry (gid 0, home /).
        let u = resolve("4242");
        assert_eq!((u.uid, u.gid), (4242, 0));
        assert_eq!(u.name, "4242");
        assert_eq!(u.home, "/");
        assert_eq!(u.groups, vec![0]);

        let err = resolve_user_from("nosuchuser", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find user 'nosuchuser'"), "{err}");
        let err = resolve_user_from("agent:nosuchgroup", PASSWD, GROUP).unwrap_err();
        assert!(err.contains("unable to find group 'nosuchgroup'"), "{err}");
    }

    #[test]
    fn survives_an_image_without_passwd() {
        // Scratch-style rootfs: numeric ids still work, names cannot.
        let u = resolve_user_from("0", "", "").unwrap();
        assert!(u.is_root());
        assert!(resolve_user_from("agent", "", "").is_err());
    }
}
