use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::SandboxId;

/// Networking profile for a sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network device and TSI disabled (fully isolated). Default.
    #[default]
    None,
    /// libkrun's native TSI backend: transparent socket impersonation —
    /// guest sockets are serviced by the host, no NIC, no extra setup.
    Tsi,
    /// Userspace NAT via gvproxy (requires the gvproxy binary). `None` = the
    /// daemon runs a private gvproxy for this sandbox; `Some(path)` = attach
    /// to a gvproxy the caller started. One gvproxy vfkit socket serves
    /// exactly one VM (see the Gvproxy docs in `manager::gvproxy`), so the
    /// managed form is the one that composes.
    Gvproxy {
        #[serde(default)]
        socket: Option<PathBuf>,
    },
    /// Attach to a pre-existing TAP device (requires privileges).
    Tap { name: String },
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkMode::None => write!(f, "none"),
            NetworkMode::Tsi => write!(f, "tsi"),
            NetworkMode::Gvproxy { .. } => write!(f, "gvproxy"),
            NetworkMode::Tap { name } => write!(f, "tap:{name}"),
        }
    }
}

impl std::str::FromStr for NetworkMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "none" => Ok(NetworkMode::None),
            "tsi" => Ok(NetworkMode::Tsi),
            "gvproxy" => Ok(NetworkMode::Gvproxy { socket: None }),
            _ if s.starts_with("gvproxy:") => Ok(NetworkMode::Gvproxy {
                socket: Some(PathBuf::from(s.trim_start_matches("gvproxy:"))),
            }),
            _ if s.starts_with("tap:") => Ok(NetworkMode::Tap {
                name: s.trim_start_matches("tap:").to_string(),
            }),
            _ => Err(format!(
                "unknown network mode '{s}' (none|tsi|gvproxy[:<socket>]|tap:<dev>)"
            )),
        }
    }
}

/// A bind mount into the sandbox (host path -> guest path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub host: PathBuf,
    pub guest: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

/// User-provided specification for a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// Optional human-friendly name.
    pub name: Option<String>,
    /// Image reference (as pulled, e.g. "alpine:latest").
    pub image: String,
    /// Command + args to run as the workload (PID 1 child of the agent).
    /// If empty, the image's Entrypoint/Cmd is used.
    #[serde(default)]
    pub command: Vec<String>,
    /// Extra environment variables (KEY=VAL).
    #[serde(default)]
    pub env: Vec<String>,
    /// Working directory inside the guest.
    pub workdir: Option<String>,
    /// Run the workload as this user (`docker run -u`), overriding the image's
    /// `USER`. Resolved in the guest against its own /etc/passwd.
    #[serde(default)]
    pub user: Option<String>,
    /// Number of vCPUs.
    #[serde(default = "default_vcpus")]
    pub vcpus: u8,
    /// RAM in MiB.
    #[serde(default = "default_ram")]
    pub ram_mib: u32,
    /// Keep the guest console's stdin open and writable through the API
    /// (`mvm run -i`). Off by default: workloads reading stdin then see
    /// immediate EOF instead of blocking forever.
    #[serde(default)]
    pub attach_stdin: bool,
    /// Run the workload on a dedicated guest pty instead of directly on the
    /// guest console (`mvm run -t`). The console itself is always a tty, but
    /// it is a shared byte stream: only a private pty gives the workload its
    /// own line discipline (echo, ^C, window size).
    #[serde(default)]
    pub tty: bool,
    /// Size (cols, rows) for that pty; without it the guest sees 0x0.
    #[serde(default)]
    pub tty_size: Option<(u16, u16)>,
    /// Network profile.
    #[serde(default)]
    pub network: NetworkMode,
    /// Port mappings "hostPort:guestPort" (only meaningful with networking).
    #[serde(default)]
    pub ports: Vec<String>,
    /// Bind mounts.
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// Labels for bookkeeping.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

fn default_vcpus() -> u8 {
    1
}
fn default_ram() -> u32 {
    512
}

impl Default for SandboxSpec {
    fn default() -> Self {
        Self {
            name: None,
            image: String::new(),
            command: vec![],
            env: vec![],
            workdir: None,
            user: None,
            vcpus: default_vcpus(),
            ram_mib: default_ram(),
            attach_stdin: false,
            tty: false,
            tty_size: None,
            network: NetworkMode::None,
            ports: vec![],
            mounts: vec![],
            labels: BTreeMap::new(),
        }
    }
}

/// Lifecycle state of a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxState {
    /// Created but never started.
    Created,
    /// VM shim process is alive.
    Running,
    /// Stopped by user request.
    Stopped,
    /// Workload exited on its own.
    Exited,
    /// Shim died unexpectedly / failed to launch.
    Failed,
}

impl SandboxState {
    pub fn is_alive(self) -> bool {
        matches!(self, SandboxState::Running)
    }
}

impl std::fmt::Display for SandboxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SandboxState::Created => "created",
            SandboxState::Running => "running",
            SandboxState::Stopped => "stopped",
            SandboxState::Exited => "exited",
            SandboxState::Failed => "failed",
        };
        f.write_str(s)
    }
}

/// Full record of a sandbox as tracked by the manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: SandboxId,
    pub spec: SandboxSpec,
    pub state: SandboxState,
    /// Exit code of the workload, if known.
    pub exit_code: Option<i32>,
    /// PID of the VM shim process on the host.
    pub pid: Option<u32>,
    /// PID of the gvproxy the daemon started for this sandbox, if any.
    /// Persisted so a restarted daemon can still reap it.
    #[serde(default)]
    pub gvproxy_pid: Option<u32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last size the console workload pty was resized to (cols, rows). The
    /// spec's `tty_size` is the create-time initial; this tracks live
    /// `/console/resize` calls and is None until one arrives.
    #[serde(default)]
    pub console_size: Option<(u16, u16)>,
    /// SHA-256 hash of the sandbox's VM-scoped bearer token, if the VM has
    /// booted. Only the hash is ever stored — the plaintext token exists
    /// solely inside the guest (provisioned over the `MVM_*` env channel)
    /// and is scrubbed from the workload environment by the agent. Regenerated
    /// on every start; revoked when the sandbox is removed.
    #[serde(default)]
    pub agent_token_hash: Option<String>,
    /// When the current token was minted (its start time).
    #[serde(default)]
    pub agent_token_created_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Sandbox {
    pub fn new(spec: SandboxSpec) -> Self {
        Self {
            id: SandboxId::new(),
            spec,
            state: SandboxState::Created,
            exit_code: None,
            pid: None,
            gvproxy_pid: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            finished_at: None,
            console_size: None,
            agent_token_hash: None,
            agent_token_created_at: None,
        }
    }

    pub fn name(&self) -> &str {
        self.spec.name.as_deref().unwrap_or(self.id.as_str())
    }
}

/// Summary of a pulled image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Reference as given by the user (e.g. "alpine:latest").
    pub reference: String,
    /// Manifest digest (sha256:...).
    pub digest: String,
    /// Total unpacked size in bytes.
    pub size: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Configuration extracted from the OCI image config blob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageConfig {
    pub env: Vec<String>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub workdir: Option<String>,
    pub user: Option<String>,
}

impl ImageConfig {
    /// Resolve the effective command for a sandbox: user override wins,
    /// then entrypoint+cmd, then /bin/sh.
    pub fn resolve_command(&self, user_command: &[String]) -> Vec<String> {
        if !user_command.is_empty() {
            return user_command.to_vec();
        }
        let mut full = self.entrypoint.clone();
        full.extend(self.cmd.clone());
        if full.is_empty() {
            full.push("/bin/sh".to_string());
        }
        full
    }
}
