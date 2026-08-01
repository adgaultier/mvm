//! Host-side write path to a connected guest agent.
//!
//! Reads are handled in `Manager::attach_agent`; writes go through an
//! unbounded channel consumed by a dedicated writer task.

use mvm_common::protocol::{encode_frame, AgentRequest};
use tokio::io::{AsyncWriteExt, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub struct AgentConn {
    tx: mpsc::UnboundedSender<AgentRequest>,
}

impl AgentConn {
    /// Spawn the writer task draining requests into the socket.
    pub fn spawn(mut writer: WriteHalf<UnixStream>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentRequest>();
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let frame = match encode_frame(&req) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("failed to encode agent request: {e}");
                        continue;
                    }
                };
                if writer.write_all(&frame).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        });
        Self { tx }
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<AgentRequest> {
        self.tx.clone()
    }
}
