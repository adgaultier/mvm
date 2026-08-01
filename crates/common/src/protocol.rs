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
    /// Spawn a process inside the sandbox.
    Exec {
        id: u32,
        argv: Vec<String>,
        env: Vec<String>,
        workdir: Option<String>,
    },
    /// stdin data for an exec session.
    Stdin { id: u32, data: String },
    /// Close stdin for an exec session.
    StdinEof { id: u32 },
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
    /// stdout data (base64-free: raw utf-8 lossy string chunk).
    Stdout { id: u32, data: String },
    /// stderr data.
    Stderr { id: u32, data: String },
    /// An exec session finished.
    Exit { id: u32, code: i32 },
    /// The main workload finished; agent will exit (VM shutdown follows).
    WorkloadExit { code: i32 },
    /// Pong reply.
    Pong,
    /// Fatal error from the agent.
    Error { message: String },
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
}
