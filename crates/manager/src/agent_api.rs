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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use mvm_common::agent_api::{
    AgentApiRequest, AgentApiResponse, AgentInfo, DelegateRequest, SetNotificationCommandParams,
};
use mvm_common::protocol::{encode_frame, MAX_FRAME};
use mvm_common::Principal;

use crate::Manager;

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
        Ok(request) => dispatch(manager, sandbox_id, &request).await,
        Err(e) => AgentApiResponse::err(format!("invalid request: {e}")),
    };

    let frame = encode_frame(&response)?;
    stream.write_all(&frame).await?;
    Ok(())
}

async fn dispatch(manager: &Manager, sandbox_id: &str, request: &AgentApiRequest) -> AgentApiResponse {
    let vm_id = match manager.authenticate_vm(&request.token) {
        Some(id) => id,
        None => return AgentApiResponse::err("invalid or expired token"),
    };
    if let Err(e) = manager.authorize(&Principal::Vm(vm_id.clone()), sandbox_id) {
        return AgentApiResponse::err(e.to_string());
    }

    match request.method.as_str() {
        "inspect" => match manager.get(vm_id.as_str()) {
            Ok(sandbox) => to_response(&AgentInfo::from(&sandbox)),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "stop" => match manager.stop(vm_id.as_str()).await {
            Ok(sandbox) => to_response(&AgentInfo::from(&sandbox)),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "delegate" => match serde_json::from_value::<DelegateRequest>(request.params.clone()) {
            Ok(_) => AgentApiResponse::err("delegate is not yet implemented"),
            Err(e) => AgentApiResponse::err(format!("invalid params: {e}")),
        },
        "set_notification_command" => {
            match serde_json::from_value::<SetNotificationCommandParams>(request.params.clone()) {
                Ok(params) => match manager.set_notification_command(vm_id.as_str(), params.command) {
                    Ok(sandbox) => {
                        tracing::info!(agent = %vm_id, "notification delivery command registered");
                        to_response(&AgentInfo::from(&sandbox))
                    }
                    Err(e) => AgentApiResponse::err(e.to_string()),
                },
                Err(e) => AgentApiResponse::err(format!("invalid params: {e}")),
            }
        }
        "ready" => match manager.mark_ready(vm_id.as_str()).await {
            Ok(sandbox) => {
                tracing::info!(agent = %vm_id, "agent marked ready");
                to_response(&AgentInfo::from(&sandbox))
            }
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "test_notification" => match manager.test_notification(vm_id.as_str()).await {
            Ok(deliveries) => to_response(&deliveries),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        other => AgentApiResponse::err(format!("unknown method '{other}'")),
    }
}

fn to_response<T: serde::Serialize>(value: &T) -> AgentApiResponse {
    match serde_json::to_value(value) {
        Ok(v) => AgentApiResponse::ok(v),
        Err(e) => AgentApiResponse::err(format!("failed to encode result: {e}")),
    }
}
