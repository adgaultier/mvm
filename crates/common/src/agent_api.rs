//! Wire types for the vsock-based Agent API (the restricted surface a
//! sandbox calls back into), distinct from the HTTP API in `api.rs` so this
//! whole module can be feature-gated (`agent-api`).
//!
//! One connection = one request + one response, length-prefixed JSON
//! (`protocol::encode_frame`), per-request over `protocol::AGENT_API_VSOCK_PORT`.
//! `token` is the VM-scoped bearer token (see `auth`), presented in the body
//! rather than a header since this isn't HTTP.

use serde::{Deserialize, Serialize};

use crate::SandboxId;

/// Agent API "delegate" params — ask the host to launch a child clone of the
/// calling sandbox, bounded by `timeout`. Not yet implemented: the handler
/// authenticates and authorizes, then reports that delegation is still in
/// progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRequest {
    /// Seconds the child may run before it is stopped.
    pub timeout: u64,
    /// Command for the child sandbox (image/env/etc. inherit from the caller;
    /// mounts are supplied by the host policy, not the caller).
    pub command: Vec<String>,
}

/// Placeholder inside a notification delivery command that the control plane
/// substitutes with the serialized `Notification` JSON when it fires one.
pub const NOTIF_MSG_PLACEHOLDER: &str = "$MSG";

/// Agent API "set_notification_command" params — register the shell command
/// template the control plane should run with `mvm exec <id> sh -c <command>`
/// to deliver async notifications to this agent. The template references
/// `$MSG` (`NOTIF_MSG_PLACEHOLDER`) for the serialized `Notification`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetNotificationCommandParams {
    /// `async_cmd` template; the control plane substitutes `$MSG` with the
    /// serialized notification at delivery time.
    pub command: String,
}

/// Agent API request envelope: the guest's `mvm-agent-mcp` bridge dials the
/// host over vsock (`protocol::AGENT_API_VSOCK_PORT`) and sends exactly one
/// of these per connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentApiRequest {
    pub method: String,
    pub token: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Agent API response envelope: exactly one per request, then the connection
/// closes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentApiResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AgentApiResponse {
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.into()),
        }
    }
}

/// A notification delivered asynchronously to a running agent — the serialized
/// `msg` of `mvm exec <async_cmd>` (typically a curl to the agent's local
/// notification endpoint). Sender and kind are the spec's `from`/`type`;
/// see `doc/agentic/notes.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Opaque unique id, for dedup and correlating replies.
    pub id: String,
    /// When the notification was emitted.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Who sent it (`from`): the parent agent, the control plane, or a child.
    pub from: NotificationFrom,
    /// What happened (`type`). Only the kinds valid for `from` are ever
    /// constructed (the `Notification::*` constructors enforce the pairing).
    #[serde(rename = "type")]
    pub kind: NotificationKind,
}

/// The sender of a notification — the spec's `from` (`daddy`,
/// `lifecycle_alert`, `child(<id>)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "kebab-case")]
pub enum NotificationFrom {
    /// The parent agent that delegated to this sandbox.
    Daddy,
    /// The mvm control plane (lifecycle events).
    LifecycleAlert,
    /// A child agent this sandbox delegated to.
    Child {
        id: SandboxId,
    },
}

/// The kind of notification — the spec's `type` (`child-ttl-about-to-expire`,
/// `restarted-after-idle`, `need-input`, `finished`, `terminated`, `input`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum NotificationKind {
    /// A child is about to hit its TTL (`lifecycle_alert`).
    ChildTtlAboutToExpire {
        /// The child whose TTL is running out.
        child: SandboxId,
        /// Seconds left before the control plane kills it.
        remaining_secs: u64,
    },
    /// The agent was restarted after an idle stop; it should continue working
    /// once its notification queue is drained (`lifecycle_alert`).
    RestartedAfterIdle,
    /// A child needs input from this agent (`child(<id>)`).
    NeedInput {
        /// What the child is waiting for (prompt, partial request, …).
        data: serde_json::Value,
    },
    /// A child finished its work (`child(<id>)`).
    Finished {
        /// The child's exit code, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Whatever the child returned (result, summary, …).
        data: serde_json::Value,
    },
    /// A child was stopped by the control plane (`child(<id>)`).
    Terminated {
        reason: TerminationReason,
    },
    /// Inbound input from the parent agent (`daddy`).
    Input {
        data: serde_json::Value,
    },
}

/// Why a child agent stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminationReason {
    /// Crashed or exited with an error.
    Faulted,
    /// Reached its TTL.
    TtlExpired,
}

impl Notification {
    /// Fresh notification with a random id and the current timestamp.
    pub fn new(from: NotificationFrom, kind: NotificationKind) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            created_at: chrono::Utc::now(),
            from,
            kind,
        }
    }

    /// `child-ttl-about-to-expire-notif` from the control plane.
    pub fn child_ttl_about_to_expire(child: SandboxId, remaining_secs: u64) -> Self {
        Self::new(
            NotificationFrom::LifecycleAlert,
            NotificationKind::ChildTtlAboutToExpire {
                child,
                remaining_secs,
            },
        )
    }

    /// `restarted-after-idle-notif` from the control plane.
    pub fn restarted_after_idle() -> Self {
        Self::new(
            NotificationFrom::LifecycleAlert,
            NotificationKind::RestartedAfterIdle,
        )
    }

    /// `need-input-notif` from a child.
    pub fn need_input(child: SandboxId, data: serde_json::Value) -> Self {
        Self::new(
            NotificationFrom::Child { id: child },
            NotificationKind::NeedInput { data },
        )
    }

    /// `finished-notif` from a child.
    pub fn finished(child: SandboxId, exit_code: Option<i32>, data: serde_json::Value) -> Self {
        Self::new(
            NotificationFrom::Child { id: child },
            NotificationKind::Finished { exit_code, data },
        )
    }

    /// `terminated-notif` from a child (faulted or ttl).
    pub fn terminated(child: SandboxId, reason: TerminationReason) -> Self {
        Self::new(
            NotificationFrom::Child { id: child },
            NotificationKind::Terminated { reason },
        )
    }

    /// `input-notif` from the parent agent.
    pub fn input(data: serde_json::Value) -> Self {
        Self::new(NotificationFrom::Daddy, NotificationKind::Input { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_roundtrips_with_spec_wire_shape() {
        let n = Notification::terminated("abc123".into(), TerminationReason::TtlExpired);
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(
            json["from"],
            serde_json::json!({ "from": "child", "id": "abc123" })
        );
        assert_eq!(
            json["type"],
            serde_json::json!({ "type": "terminated", "reason": "ttl-expired" })
        );
        let back: Notification = serde_json::from_value(json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn notification_constructors_pair_source_and_kind() {
        let child: SandboxId = "deadbeef".into();
        assert_eq!(
            Notification::need_input(child.clone(), serde_json::json!({"prompt": "go on"})).from,
            NotificationFrom::Child { id: child.clone() }
        );
        assert_eq!(
            Notification::input(serde_json::json!({"msg": "hello"})).from,
            NotificationFrom::Daddy
        );
        assert_eq!(
            Notification::child_ttl_about_to_expire(child, 30).from,
            NotificationFrom::LifecycleAlert
        );
        assert_eq!(
            Notification::restarted_after_idle().kind,
            NotificationKind::RestartedAfterIdle
        );
    }

    #[test]
    fn request_envelope_roundtrips() {
        let req = AgentApiRequest {
            method: "delegate".into(),
            token: "deadbeef".into(),
            params: serde_json::json!({ "timeout": 60, "command": ["sleep", "1"] }),
        };
        let json = serde_json::to_value(&req).unwrap();
        let back: AgentApiRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.method, "delegate");
        assert_eq!(back.params["timeout"], 60);
    }
}
