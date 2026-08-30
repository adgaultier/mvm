//! Host side of the vsock-based Agent API: one accept loop per sandbox,
//! serving the guest's `mvm-agent-mcp` bridge. Each guest connection carries
//! exactly one request and one response, length-prefixed JSON
//! (`protocol::encode_frame`) — see `mvm_common::agent_api::{AgentApiRequest,
//! AgentApiResponse}`.
//!
//! Unlike the old HTTP surface, identity here comes from two independent
//! sources that must agree: which per-sandbox unix socket accepted the
//! connection (libkrun's vsock bridge is per-VM, so only that sandbox's
//! guest can ever dial in here) and the bearer token in the request body.
//! Requiring both closes the gap the old shared-listener design had to paper
//! over with `Principal`/`authorize` alone.
//!
//! This file is pure transport + authentication; the actual request handlers
//! live in `super::handlers`.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use mvm_common::agent_api::{AgentApiRequest, AgentApiResponse};
use mvm_common::protocol::{encode_frame, MAX_FRAME};
use mvm_common::Principal;

use crate::Manager;

use super::handlers;

/// Spawn the accept loop for one sandbox's Agent API listener. Runs for the
/// sandbox's lifetime; the caller aborts it when the shim exits.
pub(crate) fn spawn_accept_loop(
    manager: Manager,
    sandbox_id: String,
    listener: UnixListener,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let manager = manager.clone();
                    let sandbox_id = sandbox_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(&manager, &sandbox_id, stream).await {
                            if is_benign_disconnect(&e) {
                                // Expected for e.g. "stop": the ack is written
                                // after the VM (and its vsock device) is
                                // already gone, so there's no client left to
                                // read it. The request itself still ran.
                                tracing::debug!(
                                    sandbox = %sandbox_id,
                                    "agent API connection closed before the response could be sent: {e}"
                                );
                            } else {
                                tracing::warn!(sandbox = %sandbox_id, "agent API request failed: {e}");
                            }
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(sandbox = %sandbox_id, "agent API accept failed: {e}");
                    break;
                }
            }
        }
    })
}

/// Whether `e` just means "the other end is gone" — normal after a
/// self-`stop`, since the guest's vsock device dies with the VM before the
/// ack can be written.
fn is_benign_disconnect(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
    )
}

async fn handle_conn(
    manager: &Manager,
    sandbox_id: &str,
    mut stream: UnixStream,
) -> std::io::Result<()> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len == 0 || len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid agent API frame length: {len}"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;

    let response = match serde_json::from_slice::<AgentApiRequest>(&payload) {
        Ok(request) => match authenticate(manager, sandbox_id, &request) {
            Ok(()) => handlers::dispatch(manager, sandbox_id, &request).await,
            Err(e) => e,
        },
        Err(e) => AgentApiResponse::err(format!("invalid request: {e}")),
    };

    let frame = encode_frame(&response)?;
    stream.write_all(&frame).await?;
    // Half-close our write end, then wait for the client to finish reading
    // and close its end before we drop the socket. A hard close right after
    // the write races libkrun's vsock bridge: the guest can see EOF before
    // the response bytes that are still in flight, losing the frame.
    stream.shutdown().await?;
    let mut drain = [0u8; 256];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.read(&mut drain).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    Ok(())
}

/// Authenticate the request: the bearer token must name a live sandbox and
/// the caller must be authorized for this target. Returns the error response
/// to send, or `Ok(())` to proceed to dispatch.
fn authenticate(manager: &Manager, sandbox_id: &str, request: &AgentApiRequest) -> Result<(), AgentApiResponse> {
    let vm_id = match manager.authenticate_vm(&request.token) {
        Some(id) => id,
        None => return Err(AgentApiResponse::err("invalid or expired token")),
    };
    if let Err(e) = manager.authorize(&Principal::Vm(vm_id), sandbox_id) {
        return Err(AgentApiResponse::err(e.to_string()));
    }
    Ok(())
}
