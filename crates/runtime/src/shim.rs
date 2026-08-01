//! The VM shim: a re-executed process that configures libkrun and enters
//! the microVM. Separating this into its own process is required because
//! `krun_start_enter` takes over the calling process (it exits when the VM
//! shuts down).

use mvm_common::{protocol, Mount, NetworkMode, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::vm::KrunContext;

/// Everything the shim needs to boot one microVM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimConfig {
    pub sandbox_id: String,
    /// Prepared root filesystem for the guest (writable).
    pub rootfs: PathBuf,
    /// Workload argv (already resolved from image config + user override).
    pub exec: Vec<String>,
    pub env: Vec<String>,
    pub workdir: Option<String>,
    pub vcpus: u8,
    pub ram_mib: u32,
    pub network: NetworkMode,
    pub ports: Vec<String>,
    pub mounts: Vec<Mount>,
    /// Host unix socket for the guest agent control channel. When set,
    /// the guest is booted with the agent as PID 1 and the workload as its
    /// child (enables `exec`).
    pub agent_socket: Option<PathBuf>,
}

impl ShimConfig {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Entry point of the `__vm-shim` subcommand. Boots the VM; only returns
/// (with an error) if the VM could not be started.
pub fn run_shim(config: &ShimConfig) -> Result<()> {
    KrunContext::set_log_level(krun_sys::KRUN_LOG_LEVEL_WARN);

    let ctx = KrunContext::new()?;
    ctx.set_vm_config(config.vcpus, config.ram_mib)?;
    ctx.set_root(&config.rootfs)?;

    // Extra virtio-fs bind mounts. Tags must be unique; the guest mounts
    // them via the agent (or they are simply visible under the tag).
    for (i, m) in config.mounts.iter().enumerate() {
        ctx.add_virtiofs(&format!("mvmfs{i}"), &m.host, m.read_only)?;
    }

    match &config.network {
        NetworkMode::None => {}
        NetworkMode::Gvproxy { socket } => {
            ctx.set_gvproxy(socket)?;
            ctx.set_port_map(&config.ports)?;
        }
        NetworkMode::Tap { name } => {
            ctx.add_net_tap(name)?;
        }
    }

    if let Some(workdir) = &config.workdir {
        ctx.set_workdir(workdir)?;
    }

    if let Some(agent_sock) = &config.agent_socket {
        // Host listens on the unix socket; the guest agent connects out.
        ctx.add_vsock_port(protocol::AGENT_VSOCK_PORT, agent_sock, false)?;
        let mut argv = vec![protocol::GUEST_AGENT_PATH.to_string()];
        argv.extend(config.exec.iter().cloned());
        // Tell the agent which virtiofs tags to mount where.
        let mut env = config.env.clone();
        if !config.mounts.is_empty() {
            let spec = config
                .mounts
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    format!(
                        "mvmfs{i}:{}{}",
                        m.guest.display(),
                        if m.read_only { ":ro" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(";");
            env.push(format!("MVM_MOUNTS={spec}"));
        }
        ctx.set_exec(protocol::GUEST_AGENT_PATH, &argv, &env)?;
    } else {
        let exec = config
            .exec
            .first()
            .cloned()
            .unwrap_or_else(|| "/bin/sh".to_string());
        ctx.set_exec(&exec, &config.exec, &config.env)?;
    }

    // Diverges on success.
    ctx.start_enter()
}
