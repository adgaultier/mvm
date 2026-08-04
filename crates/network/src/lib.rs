//! Network profile validation and helpers.
//!
//! The actual device wiring happens in the shim via libkrun
//! (gvproxy socket / TAP device / no device). This crate validates that
//! the requested profile is satisfiable on this host.

use mvm_common::{Error, NetworkMode, Result};

/// Validate that a network mode can be used right now.
pub fn validate(mode: &NetworkMode) -> Result<()> {
    match mode {
        NetworkMode::None => Ok(()),
        // TSI is provided by the libkrunfw kernel; nothing to check host-side.
        NetworkMode::Tsi => Ok(()),
        // No socket given: the daemon starts a private gvproxy per sandbox, so
        // all that must hold is that the binary is runnable.
        NetworkMode::Gvproxy { socket: None } => {
            let bin = std::env::var_os("MVM_GVPROXY_BIN")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("gvproxy"));
            if bin.components().count() > 1 {
                return if bin.exists() {
                    Ok(())
                } else {
                    Err(Error::Network(format!(
                        "MVM_GVPROXY_BIN points at {}, which does not exist",
                        bin.display()
                    )))
                };
            }
            if in_path(&bin) {
                Ok(())
            } else {
                Err(Error::Network(
                    "gvproxy not found in PATH; install it (containers/gvisor-tap-vsock), \
                     set MVM_GVPROXY_BIN, or point --net gvproxy:<socket> at a running one"
                        .into(),
                ))
            }
        }
        NetworkMode::Gvproxy { socket: Some(socket) } => {
            if socket.exists() {
                Ok(())
            } else {
                Err(Error::Network(format!(
                    "gvproxy socket {} not found; start gvproxy first (e.g. `gvproxy -listen-vfkit unixgram://{0}`) — note: libkrun speaks the vfkit datagram protocol, not -listen-qemu",
                    socket.display()
                )))
            }
        }
        NetworkMode::Tap { name } => {
            if name.is_empty() {
                return Err(Error::Network("empty TAP device name".into()));
            }
            if std::path::Path::new(&format!("/sys/class/net/{name}")).exists() {
                Ok(())
            } else {
                Err(Error::Network(format!(
                    "TAP device '{name}' does not exist; create it first (e.g. `ip tuntap add dev {name} mode tap`)"
                )))
            }
        }
    }
}

/// Is `name` an executable somewhere on PATH?
fn in_path(name: &std::path::Path) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

/// Parse a "hostPort:guestPort[/proto]" mapping (docker -p syntax, subset).
pub fn parse_port_map(s: &str) -> Result<(u16, u16)> {
    let s = s.split('/').next().unwrap_or(s);
    let (host, guest) = s
        .rsplit_once(':')
        .ok_or_else(|| Error::Network(format!("bad port mapping '{s}' (want host:guest)")))?;
    let host: u16 = host
        .parse()
        .map_err(|_| Error::Network(format!("bad host port in '{s}'")))?;
    let guest: u16 = guest
        .parse()
        .map_err(|_| Error::Network(format!("bad guest port in '{s}'")))?;
    Ok((host, guest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_maps() {
        assert_eq!(parse_port_map("8080:80").unwrap(), (8080, 80));
        assert_eq!(parse_port_map("8080:80/tcp").unwrap(), (8080, 80));
        assert!(parse_port_map("80").is_err());
        assert!(parse_port_map("x:80").is_err());
    }

    #[test]
    fn none_is_always_valid() {
        validate(&NetworkMode::None).unwrap();
    }
}
