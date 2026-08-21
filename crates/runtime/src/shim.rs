//! The VM shim: a re-executed process that configures libkrun and enters
//! the microVM. Separating this into its own process is required because
//! `krun_start_enter` takes over the calling process (it exits when the VM
//! shuts down).

use mvm_common::{protocol, Mount, NetworkMode, Result, SecurityProfile};
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
    /// Host unix socket for the guestd control channel. When set,
    /// the guest is booted with the guestd as PID 1 and the workload as its
    /// child (enables `exec`).
    pub guestd_socket: Option<PathBuf>,
    /// Host unix socket for the guest's Agent API bridge (`mvm-agent-mcp`):
    /// the guest dials out over vsock, one connection per request. Mapped
    /// only alongside `guestd_socket` (both ride the injected guestd.s
    /// boot path).
    #[serde(default)]
    pub agent_api_socket: Option<PathBuf>,
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
    /// Security profile; strict makes the guestd install an additional
    /// workload-scoped seccomp filter denying high-risk syscalls.
    #[serde(default)]
    pub security: SecurityProfile,
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

/// Host rlimit resources forwarded to the guest as
/// `(Linux guest resource id, host-side libc constant)`. The first element
/// is the *guest's* enum position and must never be taken from the host
/// constant's value: on macOS the numbering differs entirely (and some
/// resources don't exist there at all).
#[cfg(target_os = "linux")]
const HOST_RLIMITS: [(u32, u32); 16] = [
    (0, libc::RLIMIT_CPU),
    (1, libc::RLIMIT_FSIZE),
    (2, libc::RLIMIT_DATA),
    (3, libc::RLIMIT_STACK),
    (4, libc::RLIMIT_CORE),
    (5, libc::RLIMIT_RSS),
    (6, libc::RLIMIT_NPROC),
    (7, libc::RLIMIT_NOFILE),
    (8, libc::RLIMIT_MEMLOCK),
    (9, libc::RLIMIT_AS),
    (10, libc::RLIMIT_LOCKS),
    (11, libc::RLIMIT_SIGPENDING),
    (12, libc::RLIMIT_MSGQUEUE),
    (13, libc::RLIMIT_NICE),
    (14, libc::RLIMIT_RTPRIO),
    (15, libc::RLIMIT_RTTIME),
];
#[cfg(target_os = "macos")]
const HOST_RLIMITS: [(u32, libc::c_int); 10] = [
    (0, libc::RLIMIT_CPU),
    (1, libc::RLIMIT_FSIZE),
    (2, libc::RLIMIT_DATA),
    (3, libc::RLIMIT_STACK),
    (4, libc::RLIMIT_CORE),
    (5, libc::RLIMIT_RSS),
    (6, libc::RLIMIT_NPROC),
    (7, libc::RLIMIT_NOFILE),
    (8, libc::RLIMIT_MEMLOCK),
    (9, libc::RLIMIT_AS),
];

/// One `KRUN_RLIMITS` entry: `"ID=CUR:MAX"` with numeric fields — the
/// guest's `/init.krun` runs bare `strtoull` over all three. `RLIM_INFINITY`
/// becomes decimal `u64::MAX`, which `strtoull` reads back as `ULLONG_MAX`,
// i.e. exactly `RLIM_INFINITY`.
fn rlimit_entry(linux_id: u32, cur: libc::rlim_t, max: libc::rlim_t) -> String {
    let v = |x: libc::rlim_t| {
        if x == libc::RLIM_INFINITY {
            u64::MAX
        } else {
            x as u64
        }
    };
    format!("{linux_id}={}:{}", v(cur), v(max))
}

/// The shim inherits the daemon's (i.e. the host's) rlimits; forward them
/// verbatim so the guest's init — and everything it spawns: guestd,
/// workload, exec sessions — starts with the same limits.
fn host_rlimits() -> Vec<String> {
    HOST_RLIMITS
        .iter()
        .filter_map(|&(id, res)| {
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit(res as _, &mut rl) } == 0 {
                Some(rlimit_entry(id, rl.rlim_cur, rl.rlim_max))
            } else {
                None
            }
        })
        .collect()
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
    ctx.set_rlimits(&host_rlimits())?;

    // Extra virtio-fs bind mounts. Tags must be unique; the guest mounts
    // them via the guestd (or they are simply visible under the tag).
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
            let rc =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, sp.as_mut_ptr()) };
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

    if let Some(guestd_sock) = &config.guestd_socket {
        // Host listens on the unix socket; the guestd connects out.
        ctx.add_vsock_port(protocol::GUESTD_VSOCK_PORT, guestd_sock, false)?;
        if let Some(api_sock) = &config.agent_api_socket {
            // Same direction as the control channel: the guest's Agent API
            // bridge dials out, one vsock connection per request.
            ctx.add_vsock_port(protocol::AGENT_API_VSOCK_PORT, api_sock, false)?;
        }
        // libkrun's init execs KRUN_INIT with the exec path already prepended
        // as argv[0], so argv here is *just* the workload command. Repeating
        // the guestd path would make the guestd run itself as its own workload:
        // the outer instance consumed every MVM_* var (and scrubbed it from
        // the environment) before the inner one, the one that actually spawns
        // the workload and serves exec, ever saw it.
        let argv: Vec<String> = config.exec.clone();
        // Tell the guestd which virtiofs tags to mount where.
        let mut env = config.env.clone();
        // The guestd always runs inside a Linux guest, so it can't know the
        // host OS on its own; this is how macOS-specific behavior in the
        // guestd (home-ownership repair) is gated. Linux needs no signal.
        #[cfg(target_os = "macos")]
        env.push("MVM_HOST_OS=macos".to_string());
        if config.console_tty {
            env.push("MVM_CONSOLE_TTY=1".to_string());
            if let Some((cols, rows)) = config.console_size {
                env.push(format!("MVM_CONSOLE_SIZE={cols},{rows}"));
            }
        }
        match config.network {
            NetworkMode::Gvproxy { .. } => {
                // gvproxy's vfkit-mode defaults; the guestd applies them if
                // nothing else configured the interface.
                env.push("MVM_NET_CONFIG=192.168.127.2/24,192.168.127.1".to_string());
            }
            NetworkMode::Tsi => {
                // TSI needs no interface config, but images ship an empty
                // resolv.conf; the guestd fills in public resolvers.
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
        // Security profile. Strict installs an additional seccomp filter in
        // the guestd.s workload spawn path (denies bpf/keyctl/perf_event_open/
        // userfaultfd/io_uring); the guestd scrubs the var before execing.
        if config.security == SecurityProfile::Strict {
            env.push("MVM_SECURITY_STRICT=1".to_string());
        }
        // VM-scoped bearer token for the host's Agent API. It arrives here as
        // a shim *process* env var (never from shim.json, and the plaintext is
        // never persisted anywhere on the host), then rides the `MVM_*` channel
        // into the guest. Deliberately not scrubbed there: the workload's own
        // tooling (the mvm-agent-mcp bridge) presents it over the Agent API
        // vsock channel.
        if let Ok(token) = std::env::var("MVM_GUEST_TOKEN") {
            env.push(format!("MVM_GUEST_TOKEN={token}"));
        }
        ctx.set_exec(protocol::GUESTD_PATH, &argv, &env)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rlimit_entry_formats_numeric_ids_and_infinity() {
        assert_eq!(rlimit_entry(7, 1024, 4096), "7=1024:4096");
        assert_eq!(
            rlimit_entry(9, libc::RLIM_INFINITY, libc::RLIM_INFINITY),
            format!("9={}:{}", u64::MAX, u64::MAX)
        );
        assert_eq!(
            rlimit_entry(6, 256, libc::RLIM_INFINITY),
            format!("6=256:{}", u64::MAX)
        );
    }

    #[test]
    fn host_rlimits_captures_every_listed_resource() {
        let limits = host_rlimits();
        assert_eq!(limits.len(), HOST_RLIMITS.len());
        for entry in &limits {
            let (id, rest) = entry.split_once('=').expect("ID=CUR:MAX shape");
            id.parse::<u32>().expect("numeric resource id");
            let (cur, max) = rest.split_once(':').expect("CUR:MAX pair");
            cur.parse::<u64>().expect("numeric soft limit");
            max.parse::<u64>().expect("numeric hard limit");
        }
    }
}
