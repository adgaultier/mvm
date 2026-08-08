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
    /// Prepared root filesystem for the guest (writable), served over virtiofs.
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
    /// Allocate a guest PTY for the initial workload and bridge the console.
    #[serde(default)]
    pub console_tty: bool,
    /// Size (cols, rows) for that PTY.
    #[serde(default)]
    pub console_size: Option<(u16, u16)>,
    /// Identity to run the workload as (image `USER` or `-u`), resolved in the
    /// guest against its own /etc/passwd. `None` = root.
    #[serde(default)]
    pub user: Option<String>,
    /// Where libkrun's own diagnostics go. Without it they go to stderr,
    /// which for the shim *is* the guest console — hypervisor noise recorded
    /// as if the workload had printed it.
    #[serde(default)]
    pub krun_log: Option<PathBuf>,
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
    // Keep libkrun's diagnostics off the guest console. They are host-side
    // hypervisor events (`deferring proxy removal` and friends); on the
    // console they read as workload output and get recorded into
    // console.log, so `mvm logs` attributes them to the guest. The fd is
    // deliberately leaked: libkrun logs from its own threads for the whole
    // life of the VM, and this process is replaced by the VM anyway.
    let krun_log = config.krun_log.as_ref().and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    match krun_log {
        Some(file) => {
            use std::os::unix::io::IntoRawFd;
            let fd = file.into_raw_fd();
            if KrunContext::init_log_to_fd(fd, krun_sys::KRUN_LOG_LEVEL_WARN).is_err() {
                unsafe { libc::close(fd) };
                KrunContext::set_log_level(krun_sys::KRUN_LOG_LEVEL_WARN);
            }
        }
        None => KrunContext::set_log_level(krun_sys::KRUN_LOG_LEVEL_WARN),
    }

    let ctx = KrunContext::new()?;
    ctx.set_vm_config(config.vcpus, config.ram_mib)?;
    ctx.set_root(&config.rootfs)?;

    // Extra virtio-fs bind mounts. Tags must be unique; the guest mounts
    // them via the agent (or they are simply visible under the tag).
    for (i, m) in config.mounts.iter().enumerate() {
        ctx.add_virtiofs(&format!("mvmfs{i}"), &m.host, m.read_only)?;
    }

    match &config.network {
        NetworkMode::None => {
            // libkrun defaults to TSI (transparent host networking) when no
            // NIC is configured — the opposite of what "none" promises.
            // Attach a virtio-net device backed by a dead socketpair end:
            // TSI is disabled and every frame is silently dropped.
            let mut sp = [-1i32; 2];
            let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, sp.as_mut_ptr()) };
            if rc != 0 {
                return Err(mvm_common::Error::Runtime(
                    "socketpair for isolated NIC failed".into(),
                ));
            }
            // sp[1] intentionally stays open for the VM's lifetime.
            ctx.add_net_unixgram(sp[0])?;
        }
        NetworkMode::Tsi => {
            // No NIC: libkrun's default TSI backend takes over — guest
            // sockets are transparently serviced by the host. Port maps
            // ride the same mechanism.
            ctx.set_port_map(&config.ports)?;
        }
        NetworkMode::Gvproxy { socket } => {
            // The daemon resolves managed gvproxy to a concrete socket before
            // writing shim.json; nothing else can boot this VM's NIC.
            let socket = socket.as_ref().ok_or_else(|| {
                mvm_common::Error::Network("gvproxy socket missing from shim config".into())
            })?;
            ctx.set_gvproxy(socket)?;
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
        // libkrun's init execs KRUN_INIT with the exec path already prepended
        // as argv[0], so argv here is *just* the workload command. Repeating
        // the agent path would make the agent run itself as its own workload:
        // the outer instance consumed every MVM_* var (and scrubbed it from
        // the environment) before the inner one, the one that actually spawns
        // the workload and serves exec, ever saw it.
        let argv: Vec<String> = config.exec.clone();
        // Tell the agent which virtiofs tags to mount where.
        let mut env = config.env.clone();
        if config.console_tty {
            env.push("MVM_CONSOLE_TTY=1".to_string());
            if let Some((cols, rows)) = config.console_size {
                env.push(format!("MVM_CONSOLE_SIZE={cols},{rows}"));
            }
        }
        match config.network {
            NetworkMode::Gvproxy { .. } => {
                // gvproxy's vfkit-mode defaults; the agent applies them if
                // nothing else configured the interface.
                env.push("MVM_NET_CONFIG=192.168.127.2/24,192.168.127.1".to_string());
            }
            NetworkMode::Tsi => {
                // TSI needs no interface config, but images ship an empty
                // resolv.conf; the agent fills in public resolvers.
                env.push("MVM_NET_TSI=1".to_string());
            }
            _ => {}
        }
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
        // The identity is resolved in the *guest*, against the rootfs's own
        // /etc/passwd — the host has no business interpreting an image's
        // `USER` against its own user database.
        if let Some(user) = &config.user {
            env.push(format!("MVM_USER={user}"));
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
