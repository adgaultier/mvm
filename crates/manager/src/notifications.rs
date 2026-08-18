//! Control-plane notification dispatch: delivering a `Notification` to a
//! running agent by running its registered `notification_command` (`$MSG` is
//! the placeholder for the serialized notification) as `sh -c` through
//! `mvm exec`. Also the `test_notification` mock probe, which fires one
//! notification of every kind through the real delivery path — a cheap way to
//! verify a fresh agent's notification wiring end to end.

use mvm_common::agent_api::{
    Notification, NotificationKind, TerminationReason, NOTIF_MSG_PLACEHOLDER,
};
use mvm_common::protocol::GuestdEvent;
use mvm_common::{Error, Result, SandboxId};
use serde::Serialize;

use crate::Manager;

/// Report of one notification delivery through the control plane.
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
    fn succeeded(kind: String, exit_code: i32, output: String) -> Self {
        Self {
            kind,
            ok: exit_code == 0,
            exit_code: Some(exit_code),
            output,
            error: None,
        }
    }

    fn failed(kind: String, error: String) -> Self {
        Self {
            kind,
            ok: false,
            exit_code: None,
            output: String::new(),
            error: Some(error),
        }
    }
}

impl Manager {
    /// Deliver a notification to a running agent: read its registered
    /// `notification_command`, substitute `$MSG` with the serialized
    /// `Notification`, and run the template as `sh -c` via `mvm exec`.
    pub async fn deliver_notification(
        &self,
        id_or_name: &str,
        notification: &Notification,
    ) -> Result<NotificationDelivery> {
        let id = self.resolve(id_or_name)?;
        let kind = notification_kind_label(&notification.kind).to_string();
        let template = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes
                .get(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            entry.info.notification_command.clone().ok_or_else(|| {
                Error::InvalidState(format!(
                    "sandbox {id} has no registered notification command"
                ))
            })?
        };

        let msg = serde_json::to_string(notification)
            .map_err(|e| Error::Runtime(format!("serialize notification: {e}")))?;
        let command = template.replace(NOTIF_MSG_PLACEHOLDER, &msg);

        let (_session, mut rx) = self
            .exec(
                &id,
                vec!["sh".into(), "-c".into(), command],
                vec![],
                None,
                false,
                0,
                0,
                None,
            )
            .await?;

        let mut output = Vec::new();
        let mut exit_code = None;
        while let Some(event) = rx.recv().await {
            match event {
                GuestdEvent::Stdout { data, .. } | GuestdEvent::Stderr { data, .. } => {
                    output.extend_from_slice(&data);
                }
                GuestdEvent::Exit { code, .. } => {
                    exit_code = Some(code);
                    break;
                }
                GuestdEvent::Error { message } => {
                    return Err(Error::Runtime(format!("guestd: {message}")));
                }
                _ => {}
            }
        }
        let exit_code = exit_code
            .ok_or_else(|| Error::Runtime("exec channel closed before the Exit frame".into()))?;

        Ok(NotificationDelivery::succeeded(
            kind,
            exit_code,
            String::from_utf8_lossy(&output).into_owned(),
        ))
    }

    /// Fire one mock notification of every kind at the calling agent, through
    /// the same delivery path the real lifecycle events will use. Returns a
    /// per-kind report; a failure of one delivery doesn't stop the others.
    pub async fn test_notification(&self, id_or_name: &str) -> Result<Vec<NotificationDelivery>> {
        let id = self.resolve(id_or_name)?;
        let child: SandboxId = "test-child".into();
        let mocks: Vec<Notification> = vec![
            Notification::child_ttl_about_to_expire(child.clone(), 30),
            Notification::restarted_after_idle(),
            Notification::need_input(
                child.clone(),
                serde_json::json!({ "prompt": "mock need-input" }),
            ),
            Notification::finished(
                child.clone(),
                Some(0),
                serde_json::json!({ "result": "mock finished" }),
            ),
            Notification::terminated(child.clone(), TerminationReason::TtlExpired),
            Notification::input(serde_json::json!({ "text": "mock input" })),
        ];

        let mut results = Vec::with_capacity(mocks.len());
        for notification in &mocks {
            let kind = notification_kind_label(&notification.kind).to_string();
            match self.deliver_notification(&id, notification).await {
                Ok(delivery) => results.push(delivery),
                Err(e) => results.push(NotificationDelivery::failed(kind, e.to_string())),
            }
        }
        Ok(results)
    }
}

/// Kebab-case `type` label of a notification kind (the spec's `type`).
fn notification_kind_label(kind: &NotificationKind) -> &'static str {
    match kind {
        NotificationKind::ChildTtlAboutToExpire { .. } => "child-ttl-about-to-expire",
        NotificationKind::RestartedAfterIdle => "restarted-after-idle",
        NotificationKind::NeedInput { .. } => "need-input",
        NotificationKind::Finished { .. } => "finished",
        NotificationKind::Terminated { .. } => "terminated",
        NotificationKind::Input { .. } => "input",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::{DataDir, Sandbox, SandboxSpec, SandboxState};

    #[test]
    fn msg_placeholder_is_substituted_into_the_command() {
        let notification = Notification::input(serde_json::json!({ "text": "hi" }));
        let msg = serde_json::to_string(&notification).unwrap();
        let template = "echo $MSG".to_string();
        let command = template.replace(NOTIF_MSG_PLACEHOLDER, &msg);
        assert!(command.contains(&msg));
        assert!(!command.contains(NOTIF_MSG_PLACEHOLDER));
    }

    #[test]
    fn deliver_without_registered_command_is_an_error() {
        // Manager::new builds a reqwest::blocking client (ImageStore), which
        // must be constructed OUTSIDE a tokio context — so the Manager is set
        // up here and only the async delivery call runs on a runtime.
        let dir = std::env::temp_dir().join(format!("mvm-notif-deliver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = Manager::new(DataDir::at(dir.clone())).unwrap();

        let mut sb = Sandbox::new(SandboxSpec::default());
        sb.state = SandboxState::Running;
        let id = sb.id.clone();
        mgr.inner
            .sandboxes
            .write()
            .unwrap()
            .insert(id.to_string(), crate::SandboxEntry::new(sb));

        let notification = Notification::input(serde_json::json!({ "text": "hi" }));
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(mgr.deliver_notification(id.as_str(), &notification))
            .unwrap_err();
        assert!(err.to_string().contains("no registered notification command"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_notification_covers_every_kind() {
        let child: SandboxId = "test-child".into();
        let mocks = vec![
            Notification::child_ttl_about_to_expire(child.clone(), 30),
            Notification::restarted_after_idle(),
            Notification::need_input(child.clone(), serde_json::json!({})),
            Notification::finished(child.clone(), Some(0), serde_json::json!({})),
            Notification::terminated(child.clone(), TerminationReason::TtlExpired),
            Notification::input(serde_json::json!({})),
        ];
        let seen: Vec<&str> = mocks
            .iter()
            .map(|n| notification_kind_label(&n.kind))
            .collect();
        for kind in [
            "child-ttl-about-to-expire",
            "restarted-after-idle",
            "need-input",
            "finished",
            "terminated",
            "input",
        ] {
            assert!(seen.contains(&kind), "mock set missing {kind}");
        }
        assert_eq!(seen.len(), 6);
    }
}