use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use mvm_common::{Error, Result};

const GUEST_IP: &str = "192.168.127.2";

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
