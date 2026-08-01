//! Wire protocol between the host (manager) and the in-guest agent,
//! multiplexed over a single vsock-backed stream.
//!
//! Framing: [u32 big-endian length][JSON payload].
//! The same framing is reused for HTTP streaming of exec sessions.

use serde::{Deserialize, Serialize};

/// Vsock port the guest agent connects to for the control channel.
pub const AGENT_VSOCK_PORT: u32 = 1024;

/// Path of the agent inside the guest rootfs.
pub const GUEST_AGENT_PATH: &str = "/.mvm/agent";

/// Path of the ownership manifest inside the boot (virtiofs) rootfs.
/// Present when the sandbox boots from a block-device root: rootless layer
/// unpacking cannot chown, so the tar headers' owners are recorded here and
/// the agent re-applies them on the writable root disk at first boot.
pub const GUEST_OWNERSHIP_PATH: &str = "/.mvm/ownership.jsonl";

/// One line of the ownership manifest (JSON-lines format).
/// `p` is the path relative to the rootfs, `m` the full tar mode — needed
/// because chown clears setuid/setgid bits, which must be restored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipEntry {
    pub p: String,
    pub u: u32,
    pub g: u32,
    pub m: u32,
}

/// Maximum frame size (1 MiB) — guards against corrupt streams.
pub const MAX_FRAME: u32 = 1 << 20;

/// Host -> Agent messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AgentRequest {
    /// Spawn a process inside the sandbox. With `tty`, the process runs on
    /// a pseudo-terminal (`cols`/`rows` set the initial size when nonzero)
    /// and its output arrives merged on the Stdout stream.
    Exec {
        id: u32,
        argv: Vec<String>,
        env: Vec<String>,
        workdir: Option<String>,
        #[serde(default)]
        tty: bool,
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    },
    /// stdin data for an exec session (base64 over the wire: binary-safe).
    Stdin {
        id: u32,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// Close stdin for an exec session (tty sessions receive VEOF instead).
    StdinEof { id: u32 },
    /// Resize a tty exec session.
    Resize { id: u32, cols: u16, rows: u16 },
    /// Kill an exec session.
    Kill { id: u32 },
    /// Liveness probe.
    Ping,
}

/// Agent -> Host messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AgentEvent {
    /// Agent is up; carries the workload PID.
    Ready { workload_pid: u32 },
    /// stdout data (base64 over the wire: binary-safe).
    Stdout {
        id: u32,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// stderr data.
    Stderr {
        id: u32,
        #[serde(with = "b64")]
        data: Vec<u8>,
    },
    /// An exec session finished.
    Exit { id: u32, code: i32 },
    /// The main workload finished; agent will exit (VM shutdown follows).
    WorkloadExit { code: i32 },
    /// Pong reply.
    Pong,
    /// Fatal error from the agent.
    Error { message: String },
}

/// Minimal base64 (standard alphabet, with padding) so the static guest
/// agent doesn't need an extra dependency.
pub mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};

    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
            out.push(ALPHABET[idx[0] as usize] as char);
            out.push(ALPHABET[idx[1] as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[idx[2] as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[idx[3] as usize] as char } else { '=' });
        }
        out
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        fn val(c: u8) -> Result<u32, String> {
            match c {
                b'A'..=b'Z' => Ok((c - b'A') as u32),
                b'a'..=b'z' => Ok((c - b'a') as u32 + 26),
                b'0'..=b'9' => Ok((c - b'0') as u32 + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                _ => Err(format!("invalid base64 byte {c:#x}")),
            }
        }
        let s = s.as_bytes();
        if s.len() % 4 != 0 {
            return Err("base64 length not a multiple of 4".into());
        }
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for chunk in s.chunks(4) {
            let pad = chunk.iter().filter(|&&c| c == b'=').count();
            if pad > 2 || (pad > 0 && !chunk[4 - pad..].iter().all(|&c| c == b'=')) {
                return Err("malformed base64 padding".into());
            }
            let mut n = 0u32;
            for &c in &chunk[..4 - pad] {
                n = (n << 6) | val(c)?;
            }
            n <<= 6 * pad as u32;
            let bytes = n.to_be_bytes();
            out.extend_from_slice(&bytes[1..4 - pad]);
        }
        Ok(out)
    }

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        decode(&s).map_err(serde::de::Error::custom)
    }
}

/// Encode a frame: 4-byte big-endian length + JSON.
pub fn encode_frame<T: Serialize>(msg: &T) -> std::io::Result<Vec<u8>> {
    let payload = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Incremental frame decoder.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    /// Feed bytes; returns any complete frames decoded.
    pub fn feed<T: for<'de> Deserialize<'de>>(
        &mut self,
        bytes: &[u8],
    ) -> std::io::Result<Vec<T>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
            if len > MAX_FRAME {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("frame too large: {len}"),
                ));
            }
            if self.buf.len() < 4 + len as usize {
                break;
            }
            let payload: Vec<u8> = self.buf.drain(..4 + len as usize).skip(4).collect();
            let msg = serde_json::from_slice(&payload).map_err(std::io::Error::other)?;
            out.push(msg);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frame() {
        let req = AgentRequest::Exec {
            id: 7,
            argv: vec!["echo".into(), "hi".into()],
            env: vec![],
            workdir: None,
            tty: false,
            cols: 0,
            rows: 0,
        };
        let frame = encode_frame(&req).unwrap();
        let mut dec = FrameDecoder::default();
        // Feed in two chunks to test incremental decoding.
        let (a, b) = frame.split_at(3);
        assert!(dec.feed::<AgentRequest>(a).unwrap().is_empty());
        let msgs = dec.feed::<AgentRequest>(b).unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            AgentRequest::Exec { id, argv, .. } => {
                assert_eq!(*id, 7);
                assert_eq!(argv[1], "hi");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut dec = FrameDecoder::default();
        let len = (MAX_FRAME + 1).to_be_bytes();
        let r = dec.feed::<AgentRequest>(&len);
        assert!(r.is_err());
    }

    #[test]
    fn b64_roundtrips_binary() {
        for data in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foob"[..],
            &[0u8, 255, 128, 7, 0, 1][..],
        ] {
            assert_eq!(b64::decode(&b64::encode(data)).unwrap(), data);
        }
        assert_eq!(b64::encode(b"foobar"), "Zm9vYmFy");
        assert!(b64::decode("a").is_err());
        assert!(b64::decode("Zm=v").is_err());
    }

    #[test]
    fn stdout_frame_is_binary_safe() {
        let payload = vec![0u8, 159, 146, 150, 255]; // invalid UTF-8
        let event = AgentEvent::Stdout { id: 1, data: payload.clone() };
        let frame = encode_frame(&event).unwrap();
        let mut dec = FrameDecoder::default();
        let events: Vec<AgentEvent> = dec.feed(&frame).unwrap();
        match &events[0] {
            AgentEvent::Stdout { data, .. } => assert_eq!(*data, payload),
            _ => panic!("wrong variant"),
        }
    }
}
