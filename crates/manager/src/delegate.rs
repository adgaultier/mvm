//! Delegation: a running agent asks the control plane (Agent API `delegate`
//! method) to launch a child that is an *interactive clone* of itself — same
//! image, workload command, env and resources. The parent supplies *data
//! only*: the delegation `message` is queued on the child as a Daddy `input`
//! notification (`Sandbox.agent.pending_notifications`) and flushed through
//! the child's registered `agent.notification_command` once the child
//! declares `ready` — an agent can never choose a command for its child.
//! The child inherits the caller's spec, records lineage
//! (`Sandbox.agent.parent`) and starts immediately. The request's `timeout`
//! becomes the child's display-only TTL deadline — enforcement (stop+rm of
//! the agent *and* its children, per `doc/agentic/notes.md`) is a
//! follow-up.

use mvm_common::agent_api::{DelegateRequest, Notification};
use mvm_common::{Error, Result, Sandbox, SandboxId, SandboxSpec};

use crate::Manager;

/// Label carried by delegated children, value = the delegating parent's id
/// (lineage also lives on `Sandbox.agent.parent`; the label keeps it
/// visible in the plain sandbox list/inspect output).
pub const DELEGATE_PARENT_LABEL: &str = "mvm.delegate.parent";

/// Build the child's spec from the parent's record. The child runs the
/// parent's own workload (an interactive agent), so the command is inherited
/// verbatim. Pure so it can be unit-tested without booting VMs.
pub fn child_spec(parent: &Sandbox) -> SandboxSpec {
    let mut labels = parent.spec.labels.clone();
    labels.insert(DELEGATE_PARENT_LABEL.to_string(), parent.id.to_string());
    SandboxSpec {
        name: None,
        image: parent.spec.image.clone(),
        command: parent.spec.command.clone(),
        env: parent.spec.env.clone(),
        workdir: parent.spec.workdir.clone(),
        user: parent.spec.user.clone(),
        vcpus: parent.spec.vcpus,
        ram_mib: parent.spec.ram_mib,
        attach_stdin: false,
        tty: parent.spec.tty,
        tty_size: parent.spec.tty_size,
        network: parent.spec.network.clone(),
        // Deliberately not inherited: the child's gvproxy would collide with
        // the parent's host ports. Port policy for delegated children is a
        // follow-up.
        ports: Vec::new(),
        mounts: parent.spec.mounts.clone(),
        security: parent.spec.security,
        labels,
    }
}

impl Manager {
    /// Handle an Agent API `delegate` request from `caller_id`: create a
    /// clone of the caller, record lineage + TTL deadline, queue the message
    /// as a Daddy `input` notification on the child, then start it. The task
    /// is delivered by `flush_pending` once the child is ready and has
    /// registered its notification command.
    pub async fn delegate(&self, caller_id: &SandboxId, req: DelegateRequest) -> Result<Sandbox> {
        if req.message.trim().is_empty() {
            return Err(Error::Other("delegate requires a non-empty message".into()));
        }
        let caller = {
            let sandboxes = self.inner.sandboxes.read().unwrap();
            let entry = sandboxes
                .get(caller_id.as_str())
                .ok_or_else(|| Error::SandboxNotFound(caller_id.to_string()))?;
            if !entry.info.state.is_alive() {
                return Err(Error::InvalidState("only a running agent can delegate".into()));
            }
            entry.info.clone()
        };

        let child = self.create(child_spec(&caller))?;
        let task = Notification::input(serde_json::Value::String(req.message.clone()));
        {
            // Lineage, TTL and the queued task live on the record, not the spec.
            let mut sandboxes = self.inner.sandboxes.write().unwrap();
            if let Some(entry) = sandboxes.get_mut(child.id.as_str()) {
                entry.info.agent.parent = Some(caller.id.clone());
                if req.timeout > 0 {
                    entry.info.agent.ttl_deadline =
                        Some(chrono::Utc::now() + chrono::Duration::seconds(req.timeout as i64));
                }
                entry.info.agent.pending_notifications.push(task);
            }
        }
        self.persist()?;

        let started = self.start(child.id.as_str()).await?;
        tracing::info!(
            parent = %caller.id,
            child = %started.id,
            timeout_secs = req.timeout,
            "delegated child started; task queued as a daddy notification"
        );
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_common::spec::{Mount, NetworkMode};
    use mvm_common::{DataDir, SandboxState};
    use std::collections::BTreeMap;

    fn parent_sandbox() -> Sandbox {
        let mut spec = SandboxSpec::default();
        spec.image = "alpine:latest".to_string();
        spec.command = vec!["agent".to_string(), "--run".to_string()];
        spec.env = vec!["TOKEN=abc".to_string()];
        spec.workdir = Some("/work".to_string());
        spec.user = Some("1000:1000".to_string());
        spec.vcpus = 4;
        spec.ram_mib = 2048;
        spec.attach_stdin = true;
        spec.tty = true;
        spec.tty_size = Some((120, 40));
        spec.network = NetworkMode::None;
        spec.ports = vec!["8080:80".to_string()];
        spec.mounts = vec![Mount {
            host: "/host/data".into(),
            guest: "/data".into(),
            read_only: false,
        }];
        spec.labels = BTreeMap::from([("team".to_string(), "infra".to_string())]);
        Sandbox::new(spec)
    }

    #[test]
    fn child_spec_clones_parent_workload_and_adds_lineage_label() {
        let parent = parent_sandbox();
        let spec = child_spec(&parent);

        assert_eq!(spec.command, parent.spec.command, "interactive clone");
        assert_eq!(spec.image, parent.spec.image);
        assert_eq!(spec.env, parent.spec.env);
        assert_eq!(spec.workdir, parent.spec.workdir);
        assert_eq!(spec.user, parent.spec.user);
        assert_eq!(spec.vcpus, parent.spec.vcpus);
        assert_eq!(spec.ram_mib, parent.spec.ram_mib);
        assert_eq!(spec.tty, parent.spec.tty);
        assert_eq!(spec.tty_size, parent.spec.tty_size);
        assert_eq!(spec.network, parent.spec.network);
        assert_eq!(spec.mounts, parent.spec.mounts);
        assert_eq!(spec.name, None);
        assert_eq!(
            spec.labels.get(DELEGATE_PARENT_LABEL),
            Some(&parent.id.to_string())
        );
        assert_eq!(spec.labels.get("team"), Some(&"infra".to_string()));
    }

    #[test]
    fn child_spec_drops_ports_and_stdin() {
        let parent = parent_sandbox();
        let spec = child_spec(&parent);
        assert!(spec.ports.is_empty(), "child must not inherit host ports");
        assert!(!spec.attach_stdin);
    }

    #[test]
    fn delegate_with_empty_message_is_refused() {
        // Manager::new builds a reqwest::blocking client (ImageStore), which
        // must be constructed OUTSIDE a tokio context — so the Manager is set
        // up here and only the async delegate call runs on a runtime. The
        // refusal happens before any VM work, so no KVM is touched.
        let dir = std::env::temp_dir().join(format!("mvm-delegate-empty-{}", std::process::id()));
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

        let req = DelegateRequest {
            timeout: 0,
            message: "   ".to_string(),
        };
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(mgr.delegate(&id, req))
            .unwrap_err();
        assert!(err.to_string().contains("non-empty message"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
