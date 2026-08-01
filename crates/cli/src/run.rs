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
    let spec = args.spec()?;
    let sb = client.create_sandbox(&spec)?;
    let id = sb.id.to_string();

    let result = run_attached(client, &id);

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

fn run_attached(client: &Client, id: &str) -> Result<i32, String> {
    client.start_sandbox(id)?;

    // Stream logs in a background thread.
    let log_client = Client::new(client.base());
    let log_id = id.to_string();
    let log_thread = std::thread::spawn(move || {
        if let Ok(mut resp) = log_client.logs(&log_id, true) {
            let mut out = std::io::stdout();
            let _ = std::io::copy(&mut resp, &mut out);
            let _ = out.flush();
        }
    });

    // Poll until the workload exits.
    let code = loop {
        let sb = client.get_sandbox(id)?;
        if !sb.state.is_alive() {
            break sb.exit_code.unwrap_or(0);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    };

    // Give the log pump a moment to flush the tail.
    std::thread::sleep(std::time::Duration::from_millis(200));
    drop(log_thread);
    Ok(code)
}

/// Execute a command in a running sandbox, streaming framed output.
///
/// With `interactive`, local stdin is pumped to the exec session until EOF;
/// otherwise the session's stdin is closed immediately so readers don't hang.
pub fn exec(
    client: &Client,
    sandbox: &str,
    command: Vec<String>,
    interactive: bool,
) -> Result<i32, String> {
    let (session, mut resp) = client.exec(sandbox, command, vec![], None)?;

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
                    print!("{data}");
                    let _ = std::io::stdout().flush();
                }
                AgentEvent::Stderr { data, .. } => {
                    eprint!("{data}");
                    let _ = std::io::stderr().flush();
                }
                AgentEvent::Exit { code, .. } => return Ok(code),
                AgentEvent::Error { message } => return Err(message),
                _ => {}
            }
        }
    }
}
