//! Control-plane notification dispatch: delivering a `Notification` to a
//! running agent by running its registered `notification_command` (`<MSG>` is
//! the placeholder for the notification's human-readable text,
//! `Notification::to_text`) as `sh -c` through `mvm exec`. Also the
//! `test_notification` mock probe, which fires one notification of every kind
//! through the real delivery path — a cheap way to verify a fresh agent's
//! notification wiring end to end.

use mvm_common::agent_api::{Notification, NotificationDelivery, TerminationReason, MSG_PLACEHOLDER};
use mvm_common::protocol::GuestdEvent;
use mvm_common::{Error, Result, SandboxId};

use crate::Manager;

/// Cap for `Sandbox.recent_notifications` (newest wins once exceeded).
pub const MAX_RECENT_NOTIFICATIONS: usize = 16;

impl Manager {
    /// Deliver a notification to a running agent: read its registered
    /// `notification_command`, substitute `<MSG>` with the notification's
    /// human-readable text, and run the template as `sh -c` via `mvm exec`.
    pub async fn deliver_notification(
        &self,
        id_or_name: &str,
        notification: &Notification,
    ) -> Result<NotificationDelivery> {
        let id = self.resolve(id_or_name)?;
            let kind = notification.kind.label();
        let template = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes
                .get(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            entry.info.agent.notification_command.clone().ok_or_else(|| {
                Error::InvalidState(format!(
                    "sandbox {id} has no registered notification command"
                ))
            })?
        };

        let msg = notification.to_text();
        let command = template.replace(MSG_PLACEHOLDER, &msg);
        tracing::debug!(
            sandbox = %id,
            kind = %kind,
            command = %command,
            "delivering notification"
        );

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

        self.record_notification(&id, notification);

        if exit_code == 0 {
            tracing::info!(
                sandbox = %id,
                kind = %kind,
                notification = %notification.id,
                "notification delivered"
            );
        } else {
            tracing::warn!(
                sandbox = %id,
                kind = %kind,
                notification = %notification.id,
                exit_code,
                "notification command exited non-zero"
            );
        }

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
        let kind = notification.kind.label();
            match self.deliver_notification(&id, notification).await {
                Ok(delivery) => results.push(delivery),
                Err(e) => results.push(NotificationDelivery::failed(kind, e.to_string())),
            }
        }
        Ok(results)
    }

    /// Queue a notification for later delivery instead of delivering it now.
    /// Delegation uses this to hand a freshly delegated child its Daddy task
    /// before the child can receive anything; `flush_pending` drains the queue
    /// once the agent is ready. Persisted, so a daemon restart cannot lose a
    /// queued delegation.
    pub fn queue_notification(&self, id_or_name: &str, notification: Notification) -> Result<()> {
        let id = self.resolve(id_or_name)?;
        {
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            let entry = sandboxes
                .get_mut(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            entry.info.agent.pending_notifications.push(notification);
        }
        self.persist()
    }

    /// Best-effort drain of the pending queue: once the agent is running, has
    /// declared `ready`, and has registered a `notification_command`, deliver
    /// every pending notification in FIFO order through
    /// `deliver_notification`, removing each from the queue as it lands. If
    /// any precondition is missing this is a no-op (the queue stays intact for
    /// the next trigger). A delivery infrastructure error stops the flush and
    /// leaves the remainder queued; a non-zero exit of the agent's own command
    /// still counts as delivered (the agent saw it and rejected it — the
    /// delivery is recorded in the history either way).
    pub async fn flush_pending(&self, id_or_name: &str) -> Result<usize> {
        let id = self.resolve(id_or_name)?;
        let pending = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes
                .get(&id)
                .ok_or_else(|| Error::SandboxNotFound(id_or_name.to_string()))?;
            let ready = entry.info.ready_at.is_some();
            let has_command = entry.info.agent.notification_command.is_some();
            if !entry.info.state.is_alive() || !ready || !has_command {
                return Ok(0);
            }
            entry.info.agent.pending_notifications.clone()
        };
        if pending.is_empty() {
            return Ok(0);
        }
        let mut delivered = 0;
        for notification in &pending {
            self.deliver_notification(&id, notification).await?;
            delivered += 1;
            {
                let mut sandboxes = self.inner.sandboxes.write().unwrap();
                if let Some(entry) = sandboxes.get_mut(&id) {
                    entry.info.agent.pending_notifications.remove(0);
                }
            }
            self.persist()?;
        }
        tracing::info!(sandbox = %id, delivered, "pending notifications flushed");
        Ok(delivered)
    }

    /// Append a successfully delivered notification to the sandbox's volatile
    /// history (bounded, newest last). Not persisted — the history only feeds
    /// live graph views (`mvm-flow` edge labels) and resets on daemon restart.
    fn record_notification(&self, id: &str, notification: &Notification) {
        let mut sandboxes = self.inner.sandboxes.write().unwrap();
        if let Some(entry) = sandboxes.get_mut(id) {
            let history = &mut entry.info.agent.recent_notifications;
            history.push(notification.clone());
            if history.len() > MAX_RECENT_NOTIFICATIONS {
                let excess = history.len() - MAX_RECENT_NOTIFICATIONS;
                history.drain(..excess);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::agent_api::NotificationKind;
    use mvm_common::{DataDir, Sandbox, SandboxSpec, SandboxState};

    #[test]
    fn msg_placeholder_is_substituted_into_the_command() {
        let notification = Notification::input(serde_json::json!({ "text": "hi" }));
        let msg = notification.to_text();
        let template = "echo <MSG>".to_string();
        let command = template.replace(MSG_PLACEHOLDER, &msg);
        assert!(command.contains("Daddy is requesting:"));
        assert!(!command.contains(MSG_PLACEHOLDER));
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
        let mocks = [Notification::child_ttl_about_to_expire(child.clone(), 30),
            Notification::restarted_after_idle(),
            Notification::need_input(child.clone(), serde_json::json!({})),
            Notification::finished(child.clone(), Some(0), serde_json::json!({})),
            Notification::terminated(child.clone(), TerminationReason::TtlExpired),
            Notification::input(serde_json::json!({}))];
        let seen: Vec<String> = mocks.iter().map(|n| n.kind.label()).collect();
        for kind in [
            "child-ttl-about-to-expire",
            "restarted-after-idle",
            "need-input",
            "finished",
            "terminated",
            "input",
        ] {
            assert!(seen.iter().any(|s| s == kind), "mock set missing {kind}");
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn queue_notification_appends_and_persists() {
        let dir = std::env::temp_dir().join(format!("mvm-notif-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = Manager::new(DataDir::at(dir.clone())).unwrap();

        let sb = Sandbox::new(SandboxSpec::default());
        let id = sb.id.clone();
        mgr.inner
            .sandboxes
            .write()
            .unwrap()
            .insert(id.to_string(), crate::SandboxEntry::new(sb));

        mgr.queue_notification(
            id.as_str(),
            Notification::input(serde_json::json!("delegated task")),
        )
        .unwrap();

        let sandboxes = mgr.inner.sandboxes.read().unwrap();
        let pending = &sandboxes
            .get(id.as_str())
            .unwrap()
            .info
            .agent
            .pending_notifications;
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].kind,
            NotificationKind::Input {
                data: serde_json::json!("delegated task")
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_without_ready_or_command_leaves_queue_intact() {
        let dir = std::env::temp_dir().join(format!("mvm-notif-flush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = Manager::new(DataDir::at(dir.clone())).unwrap();

        let mut sb = Sandbox::new(SandboxSpec::default());
        sb.state = SandboxState::Running;
        sb.agent.notification_command = Some("true".to_string());
        // ready_at deliberately unset: the agent hasn't declared ready yet.
        let id = sb.id.clone();
        mgr.inner
            .sandboxes
            .write()
            .unwrap()
            .insert(id.to_string(), crate::SandboxEntry::new(sb));
        mgr.queue_notification(id.as_str(), Notification::input(serde_json::json!("task")))
            .unwrap();

        let flushed = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(mgr.flush_pending(id.as_str()))
            .unwrap();
        assert_eq!(flushed, 0, "nothing may be delivered before ready");
        let sandboxes = mgr.inner.sandboxes.read().unwrap();
        assert_eq!(
            sandboxes
                .get(id.as_str())
                .unwrap()
                .info
                .agent
                .pending_notifications
                .len(),
            1,
            "the queue must survive until the agent is ready"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_history_is_bounded_and_newest_last() {
        // Manager::new builds a reqwest::blocking client; construct outside tokio.
        let dir = std::env::temp_dir().join(format!("mvm-notif-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mgr = Manager::new(DataDir::at(dir.clone())).unwrap();

        let sb = Sandbox::new(SandboxSpec::default());
        let id = sb.id.clone();
        mgr.inner
            .sandboxes
            .write()
            .unwrap()
            .insert(id.to_string(), crate::SandboxEntry::new(sb));

        let total = MAX_RECENT_NOTIFICATIONS + 4;
        for i in 0..total {
            mgr.record_notification(
                id.as_str(),
                &Notification::input(serde_json::json!({ "n": i })),
            );
        }

        let sandboxes = mgr.inner.sandboxes.read().unwrap();
        let history = &sandboxes
            .get(id.as_str())
            .unwrap()
            .info
            .agent
            .recent_notifications;
        assert_eq!(history.len(), MAX_RECENT_NOTIFICATIONS);
        assert_eq!(
            history.last().unwrap().kind,
            NotificationKind::Input {
                data: serde_json::json!({ "n": (total - 1) as i32 })
            },
            "newest notification must be last"
        );
        assert_eq!(
            history.first().unwrap().kind,
            NotificationKind::Input {
                data: serde_json::json!({ "n": (total - MAX_RECENT_NOTIFICATIONS) as i32 })
            },
            "oldest entries are dropped first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}