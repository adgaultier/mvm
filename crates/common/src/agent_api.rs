//! Wire types for the vsock-based Agent API (the restricted surface a
//! sandbox calls back into), distinct from the HTTP API in `api.rs` so this
//! whole module can be feature-gated (`agent-api`).
//!
//! One connection = one request + one response, length-prefixed JSON
//! (`protocol::encode_frame`), per-request over `protocol::AGENT_API_VSOCK_PORT`.
//! `token` is the VM-scoped bearer token (see `auth`), presented in the body
//! rather than a header since this isn't HTTP.

use serde::{Deserialize, Serialize};

use crate::{Sandbox, SandboxId, SandboxState};

/// Agent API "delegate" params — ask the host to launch a child that is an
/// *interactive clone* of the calling sandbox (same image/workload/resources),
/// bounded by `timeout`. The parent supplies *data only*: the `message` is
/// queued on the child as a Daddy `input` notification
/// (`Sandbox.pending_notifications`) and delivered through the child's own
/// registered `notification_command` once the child declares `ready` — an
/// agent can never set a command for its child. The host records lineage
/// (`Sandbox.parent`) and starts the child immediately; the timeout becomes
/// the child's TTL deadline (`Sandbox.ttl_deadline`), which is display-only
/// until enforcement lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRequest {
    /// Seconds the child may run before it expires. `0` = no TTL.
    pub timeout: u64,
    /// The task handed to the child, delivered as a Daddy notification once
    /// the child is ready.
    pub message: String,
}

/// Placeholder inside a notification command template that the control plane
/// substitutes at delivery time with the notification's human-readable text
/// (`Notification::to_text`). The substitution is a literal string replace
/// and the text is prose (spaces, parentheses, …), so templates must quote
/// it — e.g. `echo '<MSG>' >> /tmp/notifs.log`.
pub const MSG_PLACEHOLDER: &str = "<MSG>";
pub const DELEGATION_PROMPT: &str= "Delegate when another agent should handle a separate task.Ask parent when you need additional information or clarification.Notify task done when you have completed your assigned task.";

/// Agent API "set_notification_command" params — register the shell command
/// template the control plane should run with `mvm exec <id> sh -c <command>`
/// to deliver async notifications to this agent. The template references
/// `<MSG>` (`MSG_PLACEHOLDER`) for the notification's text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetNotificationCommandParams {
    /// `async_cmd` template; the control plane substitutes `<MSG>` with the
    /// notification's human-readable text at delivery time.
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

/// A notification delivered asynchronously to a running agent — its
/// `to_text()` rendering becomes the `msg` of `mvm exec <async_cmd>`
/// (typically a curl to the agent's local notification endpoint). Sender and
/// kind are the spec's `from`/`type`; see `doc/agentic/notes.md`.
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

impl NotificationKind {
    /// Kebab-case `type` tag as it appears on the wire, derived from the
    /// `#[serde(tag = "type", rename_all = "kebab-case")]` representation so it
    /// can never diverge from what the guest deserializes.
    pub fn label(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.get("type").cloned())
            .and_then(|t| t.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into())
    }
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

    /// Human-readable rendering — this is what the control plane substitutes
    /// for `<MSG>` in the agent's `notification_command` at delivery time.
    /// Agents read prose, not wire formats; the JSON form stays reserved for
    /// persistence (`pending_notifications`) and host-side views.
    pub fn to_text(&self) -> String {
        // The kind determines the message; `from` only supplies the child id
        // for child-sourced kinds (the constructors enforce the pairing).
        let child_id = || match &self.from {
            NotificationFrom::Child { id } => id.to_string(),
            _ => "?".to_string(),
        };
        match &self.kind {
            NotificationKind::ChildTtlAboutToExpire {
                child,
                remaining_secs,
            } => format!("Child {child} is about to hit its TTL ({remaining_secs}s left)"),
            NotificationKind::RestartedAfterIdle => {
                "You were restarted after an idle stop; continue your work.".to_string()
            }
            NotificationKind::NeedInput { data } => {
                format!("Child {} is requesting input: {}", child_id(), render_data(data))
            }
            NotificationKind::Finished { exit_code, data } => match exit_code {
                Some(code) => format!(
                    "Child {} finished (exit code {code}): {}",
                    child_id(),
                    render_data(data)
                ),
                None => format!("Child {} finished: {}", child_id(), render_data(data)),
            },
            NotificationKind::Terminated { reason } => {
                let reason = match reason {
                    TerminationReason::Faulted => "faulted",
                    TerminationReason::TtlExpired => "TTL expired",
                };
                format!("Child {} was terminated ({reason})", child_id())
            }
            NotificationKind::Input { data } => {
                format!("{}Parent is asking:\n{}",DELEGATION_PROMPT, render_data(data))
            }
        }
    }
}

/// Report of one notification delivery through the control plane. Returned by
/// `test_notification` for the guest to verify its notification wiring.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationDelivery {
    /// Kebab-case `type` of the notification (the spec's `type`).
    pub kind: String,
    /// `true` when the notification command exited 0.
    pub ok: bool,
    /// Exit code of the notification command, when it ran.
    pub exit_code: Option<i32>,
    /// Combined stdout/stderr of the notification command.
    pub output: String,
    /// Set when the delivery itself failed (no command registered, exec
    /// error, guestd error, …) — distinct from a non-zero `exit_code`, which
    /// means the agent's endpoint saw the notification but rejected it.
    pub error: Option<String>,
}

impl NotificationDelivery {
    /// Mark a delivery as succeeded; `ok` follows the command's exit code.
    pub fn succeeded(kind: String, exit_code: i32, output: String) -> Self {
        Self {
            kind,
            ok: exit_code == 0,
            exit_code: Some(exit_code),
            output,
            error: None,
        }
    }

    /// Mark a delivery as failed on the host side (infrastructure error).
    pub fn failed(kind: String, error: String) -> Self {
        Self {
            kind,
            ok: false,
            exit_code: None,
            output: String::new(),
            error: Some(error),
        }
    }
}

/// Render a notification payload for `to_text`: strings verbatim (the common
/// case — a task or prompt), anything else as compact JSON.
fn render_data(data: &serde_json::Value) -> String {
    match data {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Redacted, agent-facing view of the calling sandbox. The Agent API must
/// never hand a workload the full `Sandbox` record: host mount paths, the
/// network profile, port mappings, host process PIDs and lifecycle telemetry
/// are control-plane internals. The agent gets its own identity, resource
/// allocation and lifecycle status, its lineage (`parent` from the record,
/// `children` computed by the manager), and the `capabilities`/`budget`
/// placeholders that stay empty until delegation policy lands.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    /// The agent's own sandbox id.
    pub id: SandboxId,
    /// The sandbox's friendly name, if set.
    pub name: Option<String>,
    /// Lifecycle status (created/running/stopped/exited/failed).
    pub state: SandboxState,
    /// When infrastructure boot completed (guestd `Ready`: seccomp, mounts,
    /// network, workload spawned, vsock control channel up). `None` until
    /// the guestd signals readiness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub booted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the workload declared itself ready (steady state: boot and
    /// runtime init complete). `None` until the agent calls the Agent API
    /// `ready` method — stays `None` for sandboxes that never call it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<chrono::DateTime<chrono::Utc>>,
    /// vCPUs allocated to the sandbox.
    pub vcpus: u8,
    /// RAM in MiB allocated to the sandbox.
    pub ram_mib: u32,
    /// The parent agent that delegated to this sandbox, if any (None = root).
    /// Set by `Manager::delegate` when the sandbox was created.
    pub parent: Option<SandboxId>,
    /// Child agents this sandbox delegated to (ids of sandboxes whose
    /// `parent` is this id). Needs registry context, so it is filled by
    /// `AgentInfo::with_lineage`, not by `From<&Sandbox>`.
    pub children: Vec<SandboxId>,
    /// Capabilities this sandbox may delegate to its children. Placeholder:
    /// always None until delegation policy is implemented.
    pub capabilities: Option<serde_json::Value>,
    /// Resource budget this sandbox may delegate to its children. Placeholder:
    /// always None until delegation policy is implemented.
    pub budget: Option<serde_json::Value>,
}

impl AgentInfo {
    /// Lineage-complete view: `children` requires registry context (a scan
    /// for sandboxes with `parent == sandbox.id`), which the manager
    /// supplies.
    pub fn with_lineage(sandbox: &Sandbox, children: Vec<SandboxId>) -> Self {
        Self {
            id: sandbox.id.clone(),
            name: sandbox.spec.name.clone(),
            state: sandbox.state,
            booted_at: sandbox.booted_at,
            ready_at: sandbox.ready_at,
            vcpus: sandbox.spec.vcpus,
            ram_mib: sandbox.spec.ram_mib,
            parent: sandbox.agent.parent.clone(),
            children,
            capabilities: None,
            budget: None,
        }
    }
}

impl From<&Sandbox> for AgentInfo {
    fn from(sandbox: &Sandbox) -> Self {
        Self::with_lineage(sandbox, Vec::new())
    }
}

/// Agent-level status derived from the sandbox record — what graph views
/// (`mvm-flow`) render in nodes. No extra state is kept: it follows `state`
/// plus `booted_at` (guestd `Ready`) and `ready_at` (Agent API `ready`).
/// `Idle` is reserved for the future idle-detection feature
/// (`doc/agentic/notes.md`, "IDLE AGENTS").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentStatus {
    /// Not running: created, stopped on request, or workload exited.
    Stopped,
    /// Shim died unexpectedly or failed to launch.
    Failed,
    /// VM shim alive, infrastructure boot not yet complete.
    Booting,
    /// Infrastructure booted, workload has not declared ready yet.
    Running,
    /// Workload declared ready (steady state).
    Ready,
    /// Reserved: running but idle (needs idle detection).
    Idle,
}

impl AgentStatus {
    pub fn derive(sandbox: &Sandbox) -> Self {
        match sandbox.state {
            SandboxState::Created | SandboxState::Stopped | SandboxState::Exited => {
                AgentStatus::Stopped
            }
            SandboxState::Failed => AgentStatus::Failed,
            SandboxState::Running => match (sandbox.booted_at, sandbox.ready_at) {
                (None, _) => AgentStatus::Booting,
                (Some(_), None) => AgentStatus::Running,
                (Some(_), Some(_)) => AgentStatus::Ready,
            },
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentStatus::Stopped => "stopped",
            AgentStatus::Failed => "failed",
            AgentStatus::Booting => "booting",
            AgentStatus::Running => "running",
            AgentStatus::Ready => "ready",
            AgentStatus::Idle => "idle",
        };
        f.write_str(s)
    }
}

/// Host-facing projection of one agent for control-plane clients (`mvm-flow`
/// over `GET /api/v1/agents`). Unlike `AgentInfo` this is for the host side
/// of the HTTP surface, so it adds the derived status, lineage, TTL deadline
/// and the latest delivered notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentView {
    pub id: SandboxId,
    pub name: Option<String>,
    pub state: SandboxState,
    pub status: AgentStatus,
    /// The agent that delegated to this one (None = root).
    pub parent: Option<SandboxId>,
    /// Direct children (ids of sandboxes whose `parent` is this id).
    pub children: Vec<SandboxId>,
    pub vcpus: u8,
    pub ram_mib: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub booted_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Display-only TTL deadline, from a `delegate` timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_deadline: Option<chrono::DateTime<chrono::Utc>>,
    /// Newest notification delivered to this agent, if any (volatile
    /// in-memory history; gone after a daemon restart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_notification: Option<Notification>,
    /// Notifications queued but not yet delivered (persisted on the sandbox
    /// record). Newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notifications: Vec<Notification>,
    /// Bounded in-memory history of delivered notifications (newest last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_notifications: Vec<Notification>,
}

impl AgentView {
    pub fn new(sandbox: &Sandbox, children: Vec<SandboxId>) -> Self {
        Self {
            id: sandbox.id.clone(),
            name: sandbox.spec.name.clone(),
            state: sandbox.state,
            status: AgentStatus::derive(sandbox),
            parent: sandbox.agent.parent.clone(),
            children,
            vcpus: sandbox.spec.vcpus,
            ram_mib: sandbox.spec.ram_mib,
            booted_at: sandbox.booted_at,
            ready_at: sandbox.ready_at,
            ttl_deadline: sandbox.agent.ttl_deadline,
            last_notification: sandbox.agent.recent_notifications.last().cloned(),
            pending_notifications: sandbox.agent.pending_notifications.clone(),
            recent_notifications: sandbox.agent.recent_notifications.clone(),
        }
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
    fn kind_label_tracks_the_wire_type_tag() {
        let child: SandboxId = "deadbeefcafe".into();
        let cases = [
            (NotificationKind::ChildTtlAboutToExpire { child, remaining_secs: 30 }, "child-ttl-about-to-expire"),
            (NotificationKind::RestartedAfterIdle, "restarted-after-idle"),
            (NotificationKind::NeedInput { data: serde_json::json!("q") }, "need-input"),
            (NotificationKind::Finished { exit_code: Some(0), data: serde_json::json!({}) }, "finished"),
            (NotificationKind::Terminated { reason: TerminationReason::TtlExpired }, "terminated"),
            (NotificationKind::Input { data: serde_json::json!({}) }, "input"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.label(), expected);
            // The label must agree with the serde-derived `type` tag the guest sees.
            let json = serde_json::to_value(&kind).unwrap();
            let tag = json["type"].as_str().unwrap();
            assert_eq!(kind.label(), tag);
        }
    }

    #[test]
    fn to_text_renders_every_kind_as_prose() {
        let child: SandboxId = "deadbeefcafe".into();
        assert_eq!(
            Notification::child_ttl_about_to_expire(child.clone(), 30).to_text(),
            "Child deadbeefcafe is about to hit its TTL (30s left)"
        );
        assert_eq!(
            Notification::restarted_after_idle().to_text(),
            "You were restarted after an idle stop; continue your work."
        );
        assert_eq!(
            Notification::need_input(child.clone(), serde_json::json!("which file?")).to_text(),
            "Child deadbeefcafe is requesting input: which file?"
        );
        assert_eq!(
            Notification::finished(child.clone(), Some(0), serde_json::json!("done")).to_text(),
            "Child deadbeefcafe finished (exit code 0): done"
        );
        assert_eq!(
            Notification::finished(child.clone(), None, serde_json::json!("done")).to_text(),
            "Child deadbeefcafe finished: done"
        );
        assert_eq!(
            Notification::terminated(child.clone(), TerminationReason::Faulted).to_text(),
            "Child deadbeefcafe was terminated (faulted)"
        );
        assert_eq!(
            Notification::terminated(child.clone(), TerminationReason::TtlExpired).to_text(),
            "Child deadbeefcafe was terminated (TTL expired)"
        );
        assert_eq!(
            Notification::input(serde_json::json!("finish the report")).to_text(),
            "Daddy is requesting: finish the report"
        );
    }

    #[test]
    fn to_text_renders_non_string_data_as_compact_json() {
        assert_eq!(
            Notification::input(serde_json::json!({ "text": "mock input" })).to_text(),
            "Daddy is requesting: {\"text\":\"mock input\"}"
        );
        // Multi-line strings stay verbatim — a delegated task can span lines.
        assert_eq!(
            Notification::input(serde_json::json!("line one\nline two")).to_text(),
            "Daddy is requesting: line one\nline two"
        );
    }

    #[test]
    fn request_envelope_roundtrips() {
        let req = AgentApiRequest {
            method: "delegate".into(),
            token: "deadbeef".into(),
            params: serde_json::json!({ "timeout": 60, "message": "finish the report" }),
        };
        let json = serde_json::to_value(&req).unwrap();
        let back: AgentApiRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.method, "delegate");
        assert_eq!(back.params["timeout"], 60);
    }

    #[test]
    fn agent_info_redacts_control_plane_fields() {
        use std::collections::BTreeMap;

        use crate::spec::{Mount, NetworkMode, Sandbox, SandboxSpec, SecurityProfile, TimelineEvent};

        let spec = SandboxSpec {
            name: Some("agent-1".to_string()),
            image: "alpine:latest".to_string(),
            command: vec!["sleep".to_string(), "infinity".to_string()],
            env: vec!["SECRET=1".to_string()],
            workdir: None,
            user: Some("1000:1000".to_string()),
            vcpus: 2,
            ram_mib: 1024,
            attach_stdin: false,
            tty: false,
            tty_size: None,
            network: NetworkMode::Gvproxy { socket: None },
            ports: vec!["8080:80".to_string()],
            mounts: vec![Mount {
                host: "/host/secret".into(),
                guest: "/guest/data".into(),
                read_only: true,
            }],
            security: SecurityProfile::Strict,
            labels: BTreeMap::from([("team".to_string(), "infra".to_string())]),
        };
        let mut sandbox = Sandbox::new(spec);
        sandbox.state = SandboxState::Running;
        sandbox.booted_at = Some(chrono::Utc::now());
        sandbox.ready_at = Some(chrono::Utc::now());
        sandbox.pid = Some(1234);
        sandbox.gvproxy_pid = Some(5678);
        sandbox.timeline.push(vec![TimelineEvent {
            event: "start_start".to_string(),
            at: chrono::Utc::now(),
        }]);
        sandbox.guest_token_hash = Some("deadbeef".to_string());

        let json = serde_json::to_value(AgentInfo::from(&sandbox)).unwrap();
        let obj = json.as_object().unwrap();
        for key in obj.keys() {
            assert!(
                [
                    "id", "name", "state", "booted_at", "ready_at", "vcpus", "ram_mib",
                    "parent", "children", "capabilities", "budget",
                ]
                .contains(&key.as_str()),
                "unexpected field in AgentInfo: {key}"
            );
        }
        assert_eq!(json["state"], serde_json::json!("running"));
        assert!(json["booted_at"].is_string());
        assert!(json["ready_at"].is_string());
        assert_eq!(json["vcpus"], 2);
        assert_eq!(json["ram_mib"], 1024);
        assert_eq!(json["name"], serde_json::json!("agent-1"));
        assert_eq!(json["parent"], serde_json::Value::Null);
        assert_eq!(json["children"], serde_json::json!([]));
        assert_eq!(json["capabilities"], serde_json::Value::Null);
        assert_eq!(json["budget"], serde_json::Value::Null);

        let serialized = json.to_string();
        for leaked in [
            "/host/secret",
            "8080:80",
            "SECRET=1",
            "deadbeef",
            "gvproxy_pid",
            "lifecycle",
        ] {
            assert!(
                !serialized.contains(leaked),
                "AgentInfo leaked control-plane field '{leaked}'"
            );
        }
    }

    fn sandbox_with(state: SandboxState, booted: bool, ready: bool) -> Sandbox {
        use crate::spec::{NetworkMode, SandboxSpec, SecurityProfile};

        let spec = SandboxSpec {
            name: None,
            image: "alpine:latest".to_string(),
            command: vec![],
            env: vec![],
            workdir: None,
            user: None,
            vcpus: 1,
            ram_mib: 512,
            attach_stdin: false,
            tty: false,
            tty_size: None,
            network: NetworkMode::None,
            ports: vec![],
            mounts: vec![],
            security: SecurityProfile::Default,
            labels: std::collections::BTreeMap::new(),
        };
        let mut sb = Sandbox::new(spec);
        sb.state = state;
        if booted {
            sb.booted_at = Some(chrono::Utc::now());
        }
        if ready {
            sb.ready_at = Some(chrono::Utc::now());
        }
        sb
    }

    #[test]
    fn agent_status_derives_from_lifecycle_fields() {
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Created, false, false)),
            AgentStatus::Stopped
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Stopped, true, true)),
            AgentStatus::Stopped
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Exited, false, false)),
            AgentStatus::Stopped
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Failed, false, false)),
            AgentStatus::Failed
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Running, false, false)),
            AgentStatus::Booting
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Running, false, true)),
            AgentStatus::Booting,
            "ready without booted is still booting"
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Running, true, false)),
            AgentStatus::Running
        );
        assert_eq!(
            AgentStatus::derive(&sandbox_with(SandboxState::Running, true, true)),
            AgentStatus::Ready
        );
    }

    #[test]
    fn agent_view_carries_lineage_and_ttl() {
        let parent = sandbox_with(SandboxState::Running, true, true);
        let mut child = sandbox_with(SandboxState::Running, true, false);
        child.agent.parent = Some(parent.id.clone());
        child.agent.ttl_deadline = Some(chrono::Utc::now() + chrono::Duration::seconds(60));
        child.agent.recent_notifications.push(Notification::input(serde_json::json!({"msg": "hi"})));

        let view = AgentView::new(&child, vec![]);
        assert_eq!(view.status, AgentStatus::Running);
        assert_eq!(view.parent, Some(parent.id.clone()));
        assert!(view.ttl_deadline.is_some());
        assert!(view.last_notification.is_some());

        let parent_view = AgentView::new(&parent, vec![child.id.clone()]);
        assert_eq!(parent_view.children, vec![child.id.clone()]);
        assert_eq!(parent_view.parent, None);

        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["status"], serde_json::json!("running"));
        assert_eq!(json["parent"], serde_json::json!(parent.id.as_str()));
    }
}
