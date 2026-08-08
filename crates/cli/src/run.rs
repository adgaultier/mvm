//! Higher-level CLI flows: pull progress, run, exec.

use std::io::{Read, Write};

use mvm_common::protocol::{AgentEvent, FrameDecoder};

use crate::client::Client;
use crate::BoxArgs;

/// Pull an image, rendering progress from the daemon's JSON-line stream.
pub fn pull(client: &Client, reference: &str) -> Result<i32, String> {
    let mut resp = client.pull(reference)?;
    let mut buf = String::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = resp.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            print_pull_event(line.trim());
        }
    }
    if !buf.trim().is_empty() {
        print_pull_event(buf.trim());
    }
    Ok(0)
}

fn print_pull_event(line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let short = |d: &str| d.chars().take(19).collect::<String>();
    match v.get("stage").and_then(|s| s.as_str()) {
        Some("manifest") => {
            println!("pulling {}", v["reference"].as_str().unwrap_or_default());
        }
        Some("layerstart") => {
            println!(
                "  layer {} ({} bytes)",
                short(v["digest"].as_str().unwrap_or("")),
                v["size"].as_u64().unwrap_or(0)
            );
        }
        Some("unpacking") => {
            println!("  unpack {}", short(v["digest"].as_str().unwrap_or("")));
        }
        Some("pulled") => {
            println!(
                "pulled {} ({})",
                v["reference"].as_str().unwrap_or_default(),
                short(v["digest"].as_str().unwrap_or(""))
            );
        }
        Some("uptodate") => {
            println!(
                "up to date ({})",
                short(v["digest"].as_str().unwrap_or(""))
            );
        }
        Some("error") => {
            eprintln!("pull error: {}", v["error"].as_str().unwrap_or_default());
        }
        _ => {}
    }
}

/// create + start + stream logs + wait + cleanup.
pub fn run(client: &Client, args: BoxArgs) -> Result<i32, String> {
    let keep = args.keep;
    let interactive = args.interactive;
    let tty = args.tty;
    let spec = args.spec()?;
    let sb = client.create_sandbox(&spec)?;
    let id = sb.id.to_string();

    let result = run_attached(client, &id, interactive, tty);

    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            let _ = client.stop_sandbox(&id);
            if !keep {
                let _ = client.remove_sandbox(&id);
            }
            return Err(e);
        }
    };

    if !keep {
        client.remove_sandbox(&id)?;
        // A name only outlives the run with --keep. Say so, or `mvm start
        // <name>` afterwards just reports "sandbox not found".
        if let Some(name) = &spec.name {
            eprintln!("mvm: sandbox '{name}' removed on exit (use --keep to start it again)");
        }
    } else {
        println!("sandbox {id} kept (state: exited)");
    }
    Ok(exit_code)
}

fn run_attached(client: &Client, id: &str, interactive: bool, tty: bool) -> Result<i32, String> {
    client.start_sandbox(id)?;
    // No detach keys here: `run` owns the sandbox's lifetime (it removes it
    // unless --keep), so leaving the workload behind would orphan it. And no
    // backlog cap — this console starts empty.
    console_session(client, id, interactive, tty, None, None)
}

/// Attach to the console of an already-running sandbox, docker-attach style.
///
/// Whether stdin can be forwarded is fixed at creation (`-i` opens the
/// console's write end in the daemon), as is whether the workload sits on a
/// guest pty (`-t`); this reads both off the spec rather than taking flags
/// that could contradict the VM it is attaching to.
pub fn attach(client: &Client, sandbox: &str, no_stdin: bool) -> Result<i32, String> {
    let sb = client.get_sandbox(sandbox)?;
    let id = sb.id.to_string();
    if !sb.state.is_alive() {
        return Err(format!(
            "sandbox {} is {} — start it first (`mvm start -a {sandbox}` does both)",
            sb.name(),
            sb.state
        ));
    }

    let interactive = !no_stdin && sb.spec.attach_stdin;
    if !no_stdin && !sb.spec.attach_stdin {
        eprintln!(
            "mvm: {} was created without -i, so its console stdin is closed; attaching read-only",
            sb.name()
        );
    }
    // The escape sequence only works when we own a raw local terminal and can
    // actually send keys; otherwise ^C is the only way out and saying
    // "ctrl-p ctrl-q" would be a lie.
    let local_tty = unsafe { libc::isatty(0) == 1 };
    let detach = (interactive && sb.spec.tty && local_tty).then_some(DETACH_KEYS);
    eprintln!(
        "mvm: attached to {} — detach with {}",
        sb.name(),
        if detach.is_some() { "ctrl-p ctrl-q" } else { "ctrl-c" }
    );
    console_session(
        client,
        &id,
        interactive,
        sb.spec.tty,
        detach,
        Some(ATTACH_TAIL_LINES),
    )
}

/// ^P ^Q — docker's default detach sequence.
const DETACH_KEYS: [u8; 2] = [0x10, 0x11];

/// Bridge the local terminal to a running sandbox's console until the VM
/// exits (or the user types `detach`, when set), then report the exit code.
fn console_session(
    client: &Client,
    id: &str,
    interactive: bool,
    tty: bool,
    detach: Option<[u8; 2]>,
    backlog_tail: Option<usize>,
) -> Result<i32, String> {
    // Raw mode while attached (interactive tty runs); restored by Drop.
    // The guest console is a tty with echo, so keystrokes render from the
    // guest side, docker-style.
    let _raw = if tty && interactive {
        RawTermGuard::enable()
    } else {
        None
    };
    let raw_active = _raw.is_some();

    // Pump local stdin into the guest console.
    if interactive {
        let stdin_client = Client::new(client.base());
        let sid = id.to_string();
        let name = id.to_string();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut chunk = [0u8; 4096];
            // Detach scanning only makes sense on a raw tty; a pipe must stay
            // byte-exact (binaries travel through here).
            let mut scanner = detach
                .filter(|_| raw_active)
                .map(DetachScanner::new);
            loop {
                let n = match stdin.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let payload = match scanner.as_mut() {
                    None => chunk[..n].to_vec(),
                    Some(scanner) => match scanner.feed(&chunk[..n]) {
                        Scanned::Pass(bytes) => bytes,
                        Scanned::Detached(bytes) => {
                            if !bytes.is_empty() {
                                let _ = stdin_client.sandbox_stdin(&name, bytes);
                            }
                            // The log stream blocks in the main thread and
                            // cannot be interrupted, so leave from here —
                            // after putting the terminal back by hand, since
                            // exiting skips the guard's Drop.
                            restore_terminal();
                            eprintln!("\r\nmvm: detached (sandbox still running)");
                            std::process::exit(0);
                        }
                    },
                };
                if payload.is_empty() {
                    continue;
                }
                if stdin_client.sandbox_stdin(&name, payload).is_err() {
                    break;
                }
            }
            let _ = stdin_client.sandbox_stdin_eof(&sid);
        });
    }

    // The follow stream carries the whole console and ends when the VM
    // exits (the daemon closes the channel on shim exit), so streaming it
    // to EOF *is* waiting for the workload — no polling, no lost tail.
    //
    // Flush after every chunk: stdout is a LineWriter, so `io::copy` would
    // hold anything not ending in '\n' — including shell prompts and, in
    // raw mode where there is no local echo, every keystroke echoed back by
    // the guest. That looks exactly like a hung terminal.
    //
    // An attach replays only the last screenful of console: enough to land on
    // a prompt without dumping the whole history of a long-lived sandbox.
    //
    // `raw`: this is the one consumer that must see terminal queries. The
    // guest's shell asks for the cursor column and expects the answer back
    // on its stdin, which is exactly what an attached terminal provides.
    let mut resp = client.logs(id, true, backlog_tail, true)?;
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }

    // The daemon's child watcher records the exit code at roughly the same
    // moment the stream ends; allow it a bounded beat to land.
    for _ in 0..200 {
        let sb = client.get_sandbox(id)?;
        if !sb.state.is_alive() {
            return Ok(sb.exit_code.unwrap_or(0));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err("sandbox still running after log stream ended".into())
}

/// How much console history an `attach` replays before going live.
const ATTACH_TAIL_LINES: usize = 40;

/// Watches stdin for the detach sequence, holding back a partial match so the
/// first key never reaches the guest on its own. A doubled first key (^P^P)
/// passes one literal through, as docker does.
struct DetachScanner {
    keys: [u8; 2],
    pending: bool,
}

enum Scanned {
    /// Bytes to forward.
    Pass(Vec<u8>),
    /// Detach requested; forward these bytes first, then leave.
    Detached(Vec<u8>),
}

impl DetachScanner {
    fn new(keys: [u8; 2]) -> Self {
        Self { keys, pending: false }
    }

    fn feed(&mut self, input: &[u8]) -> Scanned {
        let mut out = Vec::with_capacity(input.len() + 1);
        for &b in input {
            if self.pending {
                self.pending = false;
                if b == self.keys[1] {
                    return Scanned::Detached(out);
                }
                out.push(self.keys[0]);
                if b == self.keys[0] {
                    continue; // ^P^P -> one literal ^P
                }
                out.push(b);
            } else if b == self.keys[0] {
                self.pending = true;
            } else {
                out.push(b);
            }
        }
        Scanned::Pass(out)
    }
}

/// The terminal settings to put back, shared because a detach leaves from a
/// helper thread and never runs the guard's `Drop`.
static ORIGINAL_TERMIOS: std::sync::Mutex<Option<libc::termios>> = std::sync::Mutex::new(None);

/// Undo raw mode, once, whoever gets there first.
fn restore_terminal() {
    let Ok(mut saved) = ORIGINAL_TERMIOS.lock() else { return };
    if let Some(term) = saved.take() {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &term);
        }
    }
}

/// Restores the local terminal on drop (raw mode for `exec -it`).
struct RawTermGuard;

impl RawTermGuard {
    /// Put the local terminal into raw mode; None when stdin isn't a tty.
    fn enable() -> Option<Self> {
        unsafe {
            if libc::isatty(0) == 0 {
                return None;
            }
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut term) != 0 {
                return None;
            }
            let orig = term;
            // Fully raw, both directions: raw mode only engages with `-t`,
            // and then the guest workload runs on a guest pty whose line
            // discipline already emits CRLF. Leaving OPOST/ONLCR on here
            // would translate that '\n' a second time.
            libc::cfmakeraw(&mut term);
            if libc::tcsetattr(0, libc::TCSANOW, &term) != 0 {
                return None;
            }
            *ORIGINAL_TERMIOS.lock().ok()? = Some(orig);
            Some(Self)
        }
    }
}

impl Drop for RawTermGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Local terminal size, if any std fd is a tty.
pub fn term_size() -> Option<(u16, u16)> {
    for fd in [0, 1, 2] {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            return Some((ws.ws_col, ws.ws_row));
        }
    }
    None
}

/// Execute a command in a running sandbox, streaming framed output.
///
/// With `interactive`, local stdin is pumped to the exec session until EOF;
/// otherwise the session's stdin is closed immediately so readers don't hang.
/// With `tty`, the guest process runs on a pseudo-terminal; when the local
/// stdin is also a terminal, it is switched to raw mode so keystrokes
/// (including ^C, arrows, etc.) pass through to the guest.
pub fn exec(
    client: &Client,
    sandbox: &str,
    command: Vec<String>,
    interactive: bool,
    tty: bool,
    user: Option<String>,
) -> Result<i32, String> {
    let (cols, rows) = if tty { term_size().unwrap_or((0, 0)) } else { (0, 0) };
    let (session, mut resp) =
        client.exec(sandbox, command, vec![], None, tty, cols, rows, user)?;

    // Raw mode while the session runs; restored by Drop on every exit path.
    let _raw = if tty && interactive {
        RawTermGuard::enable()
    } else {
        None
    };

    // Track local terminal resizes (poll: no signal handler needed).
    if tty && term_size().is_some() {
        let resize_client = Client::new(client.base());
        let sb = sandbox.to_string();
        std::thread::spawn(move || {
            let mut last = (cols, rows);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                match term_size() {
                    Some(size) if size != last => {
                        if resize_client.exec_resize(&sb, session, size.0, size.1).is_err() {
                            break;
                        }
                        last = size;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        });
    }

    if interactive {
        let stdin_client = Client::new(client.base());
        let stdin_sandbox = sandbox.to_string();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut chunk = [0u8; 4096];
            loop {
                match stdin.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stdin_client
                            .exec_stdin(&stdin_sandbox, session, chunk[..n].to_vec())
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let _ = stdin_client.exec_stdin_eof(&stdin_sandbox, session);
        });
    } else {
        client.exec_stdin_eof(sandbox, session)?;
    }

    let mut decoder = FrameDecoder::default();
    let mut buf = [0u8; 8192];
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            // Stream ended without an Exit frame.
            return Ok(1);
        }
        let events: Vec<AgentEvent> = decoder.feed(&buf[..n]).map_err(|e| e.to_string())?;
        for event in events {
            match event {
                AgentEvent::Stdout { data, .. } => {
                    let mut out = std::io::stdout();
                    let _ = out.write_all(&data);
                    let _ = out.flush();
                }
                AgentEvent::Stderr { data, .. } => {
                    let mut err = std::io::stderr();
                    let _ = err.write_all(&data);
                    let _ = err.flush();
                }
                AgentEvent::Exit { code, .. } => return Ok(code),
                AgentEvent::Error { message } => return Err(message),
                _ => {}
            }
        }
    }
}
