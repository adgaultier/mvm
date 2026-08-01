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
    } else {
        println!("sandbox {id} kept (state: exited)");
    }
    Ok(exit_code)
}

fn run_attached(client: &Client, id: &str, interactive: bool, tty: bool) -> Result<i32, String> {
    client.start_sandbox(id)?;

    // Raw mode while attached (interactive tty runs); restored by Drop.
    // The guest console is a tty with echo, so keystrokes render from the
    // guest side, docker-style.
    let _raw = if tty && interactive {
        RawTermGuard::enable()
    } else {
        None
    };

    // Pump local stdin into the guest console.
    if interactive {
        let stdin_client = Client::new(client.base());
        let sid = id.to_string();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut chunk = [0u8; 4096];
            loop {
                match stdin.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stdin_client.sandbox_stdin(&sid, chunk[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = stdin_client.sandbox_stdin_eof(&sid);
        });
    }

    // The follow stream carries the whole console and ends when the VM
    // exits (the daemon closes the channel on shim exit), so streaming it
    // to EOF *is* waiting for the workload — no polling, no lost tail.
    let mut resp = client.logs(id, true)?;
    let mut out = std::io::stdout();
    std::io::copy(&mut resp, &mut out).map_err(|e| e.to_string())?;
    let _ = out.flush();

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

/// Restores the local terminal on drop (raw mode for `exec -it`).
struct RawTermGuard(libc::termios);

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
            libc::cfmakeraw(&mut term);
            if libc::tcsetattr(0, libc::TCSANOW, &term) != 0 {
                return None;
            }
            Some(Self(orig))
        }
    }
}

impl Drop for RawTermGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

/// Local terminal size, if any std fd is a tty.
fn term_size() -> Option<(u16, u16)> {
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
) -> Result<i32, String> {
    let (cols, rows) = if tty { term_size().unwrap_or((0, 0)) } else { (0, 0) };
    let (session, mut resp) = client.exec(sandbox, command, vec![], None, tty, cols, rows)?;

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
