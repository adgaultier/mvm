//! Dispatch layer for the Agent API: the wire-method dispatch (`dispatch`)
//! and the per-kind notification handlers. Keeping both here centralizes the
//! "what does this request mean" logic — `agent_api` just moves bytes over the
//! vsock/unix-socket bridge, `handlers` decides what happens.
//!
//! The per-kind handlers are deliberately unimplemented stubs (`todo!()`):
//! the control-plane reaction to each notification kind (task done, request
//! for info, TTL expiry, …) is still being designed. They are wired into
//! dispatch so a handler slot exists for every kind and the match stays
//! exhaustive.

use mvm_common::agent_api::{
    AgentApiRequest, AgentApiResponse, AgentInfo, DelegateRequest, NotificationKind,
    SetNotificationCommandParams,
};

use crate::Manager;

/// Dispatch one Agent API request. Authentication (token + principal) has
/// already happened in `agent_api::handle_conn`; here we only pick a handler.
pub(crate) async fn dispatch(
    manager: &Manager,
    sandbox_id: &str,
    request: &AgentApiRequest,
) -> AgentApiResponse {
    match request.method.as_str() {
        "inspect" => match manager.get(sandbox_id) {
            Ok(sandbox) => to_response(&AgentInfo::with_lineage(
                &sandbox,
                manager.children_of(&sandbox.id),
            )),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "stop" => match manager.stop(sandbox_id).await {
            Ok(sandbox) => to_response(&AgentInfo::from(&sandbox)),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "delegate" => match serde_json::from_value::<DelegateRequest>(request.params.clone()) {
            Ok(req) => match manager.delegate(&sandbox_id.into(), req).await {
                Ok(child) => to_response(&AgentInfo::with_lineage(
                    &child,
                    manager.children_of(&child.id),
                )),
                Err(e) => AgentApiResponse::err(e.to_string()),
            },
            Err(e) => AgentApiResponse::err(format!("invalid params: {e}")),
        },
        "set_notification_command" => {
            match serde_json::from_value::<SetNotificationCommandParams>(request.params.clone()) {
                Ok(params) => {
                    match manager.set_notification_command(sandbox_id, params.command) {
                        Ok(sandbox) => {
                            tracing::info!(agent = %sandbox_id, "notification delivery command registered");
                            // Race guard: if the agent declared `ready` before
                            // registering the command, `mark_ready`'s flush found
                            // nothing to drain — do it now.
                            if let Err(e) = manager.flush_pending(sandbox_id).await {
                                tracing::warn!(
                                    agent = %sandbox_id,
                                    error = %e,
                                    "flushing pending notifications failed"
                                );
                            }
                            to_response(&AgentInfo::from(&sandbox))
                        }
                        Err(e) => AgentApiResponse::err(e.to_string()),
                    }
                }
                Err(e) => AgentApiResponse::err(format!("invalid params: {e}")),
            }
        }
        "ready" => match manager.mark_ready(sandbox_id).await {
            Ok(sandbox) => {
                tracing::info!(agent = %sandbox_id, "agent marked ready");
                to_response(&AgentInfo::from(&sandbox))
            }
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        "test_notification" => match manager.test_notification(sandbox_id).await {
            Ok(deliveries) => to_response(&deliveries),
            Err(e) => AgentApiResponse::err(e.to_string()),
        },
        other => AgentApiResponse::err(format!("unknown method '{other}'")),
    }
}

/// Run the control-plane handler for an emitted notification kind. Each kind
/// gets a dedicated handler slot; the bodies are `todo!()` until the
/// per-kind reactions are designed. `_sandbox_id` is the agent the
/// notification is about (the one that will see it / act on it).
///
/// Not yet called from the delivery path — the wiring is deliberately a
/// follow-up — hence `allow(dead_code)` until then.
#[allow(dead_code)]
pub(crate) fn handle_notification(sandbox_id: &str, kind: &NotificationKind) {
    match kind {
        NotificationKind::ChildTtlAboutToExpire { child, remaining_secs } => {
            on_child_ttl_about_to_expire(sandbox_id, child.as_str(), *remaining_secs)
        }
        NotificationKind::RestartedAfterIdle => on_restarted_after_idle(sandbox_id),
        NotificationKind::NeedInput { .. } => on_need_input(sandbox_id, kind),
        NotificationKind::Finished { .. } => on_finished(sandbox_id, kind),
        NotificationKind::Terminated { .. } => on_terminated(sandbox_id, kind),
        NotificationKind::Input { .. } => on_input(sandbox_id, kind),
    }
}

fn on_child_ttl_about_to_expire(_sandbox_id: &str, _child: &str, _remaining_secs: u64) {
    todo!("decide how a parent reacts to a child about to expire its TTL")
}
fn on_restarted_after_idle(_sandbox_id: &str) {
    todo!("decide how an agent resumes after an idle restart")
}
fn on_need_input(_sandbox_id: &str, _kind: &NotificationKind) {
    todo!("decide how the parent acts on a child asking for input")
}
fn on_finished(_sandbox_id: &str, _kind: &NotificationKind) {
    todo!("decide how the parent reacts to a child finishing its task (task done)")
}
fn on_terminated(_sandbox_id: &str, _kind: &NotificationKind) {
    todo!("decide how the parent reacts to a child being stopped by the control plane")
}
fn on_input(_sandbox_id: &str, _kind: &NotificationKind) {
    todo!("decide how an agent processes inbound input from its parent")
}

fn to_response<T: serde::Serialize>(value: &T) -> AgentApiResponse {
    match serde_json::to_value(value) {
        Ok(v) => AgentApiResponse::ok(v),
        Err(e) => AgentApiResponse::err(format!("failed to encode result: {e}")),
    }
}
