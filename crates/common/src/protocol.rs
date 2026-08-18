//! Wire protocol between the host (manager) and the in-guestd,
//! multiplexed over a single vsock-backed stream.
//!
//! Framing: [u32 big-endian length][JSON payload].
//! The same framing is reused for HTTP streaming of exec sessions.

use serde::{Deserialize, Serialize};

/// Vsock port the guestd connects to for the control channel.
pub const GUESTD_VSOCK_PORT: u32 = 1024;

/// Vsock port the guest's Agent API bridge (`mvm-agent-mcp`) dials to reach
/// the host's per-sandbox Agent API listener. Mapped alongside the control
/// channel (same host unix-socket-backed vsock mechanism, opposite
/// direction: the guest connects out, one connection per request).
pub const AGENT_API_VSOCK_PORT: u32 = 24643;

/// Path of the guestd inside the guest rootfs.
pub const GUESTD_PATH: &str = "/.mvm/guestd";

/// Maximum frame size (1 MiB) — guards against corrupt streams.
pub const MAX_FRAME: u32 = 1 << 20;

/// Host -> Guestd messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GuestdRequest {
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
        /// Run as this user instead of the workload's own identity
        /// (`docker exec -u`); `None` = whoever the workload runs as.
        #[serde(default)]
        user: Option<String>,
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
    /// Resize the console workload's pty (sandbox-keyed rather than
    /// session-keyed, so it has no `id`).
    ConsoleResize { cols: u16, rows: u16 },
    /// Kill an exec session.
    Kill { id: u32 },
    /// Liveness probe.
    Ping,
}

/// Guestd -> Host messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GuestdEvent {
    /// Guestd is up; carries the workload PID.
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
    /// The main workload finished; guestd will exit (VM shutdown follows).
    WorkloadExit { code: i32 },
    /// Pong reply.
    Pong,
    /// Fatal error from the guestd.
    Error { message: String },
}

/// Base64 (standard alphabet, with padding) for byte payloads on the wire.
/// Uses the `base64` crate; the module stays so `#[serde(with = "b64")]`
/// keeps working for the framed JSON structs.
pub mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    const STANDARD: base64::engine::general_purpose::GeneralPurpose =
        base64::engine::general_purpose::STANDARD;

    pub fn encode(data: &[u8]) -> String {
        STANDARD.encode(data)
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        STANDARD.decode(s).map_err(|e| e.to_string())
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
    pub fn feed<T: for<'de> Deserialize<'de>>(&mut self, bytes: &[u8]) -> std::io::Result<Vec<T>> {
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
        let req = GuestdRequest::Exec {
            id: 7,
            argv: vec!["echo".into(), "hi".into()],
            env: vec![],
            workdir: None,
            tty: false,
            cols: 0,
            rows: 0,
            user: None,
        };
        let frame = encode_frame(&req).unwrap();
        let mut dec = FrameDecoder::default();
        // Feed in two chunks to test incremental decoding.
        let (a, b) = frame.split_at(3);
        assert!(dec.feed::<GuestdRequest>(a).unwrap().is_empty());
        let msgs = dec.feed::<GuestdRequest>(b).unwrap();
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            GuestdRequest::Exec { id, argv, .. } => {
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
        let r = dec.feed::<GuestdRequest>(&len);
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
        let event = GuestdEvent::Stdout {
            id: 1,
            data: payload.clone(),
        };
        let frame = encode_frame(&event).unwrap();
        let mut dec = FrameDecoder::default();
        let events: Vec<GuestdEvent> = dec.feed(&frame).unwrap();
        match &events[0] {
            GuestdEvent::Stdout { data, .. } => assert_eq!(*data, payload),
            _ => panic!("wrong variant"),
        }
    }
}
