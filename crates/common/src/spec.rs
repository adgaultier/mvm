use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::SandboxId;

/// Networking profile for a sandbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network device at all (fully isolated). Default.
    #[default]
    None,
    /// Userspace NAT via gvproxy (requires gvproxy binary).
    Gvproxy { socket: PathBuf },
    /// Attach to a pre-existing TAP device (requires privileges).
    Tap { name: String },
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkMode::None => write!(f, "none"),
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
            "gvproxy" => Ok(NetworkMode::Gvproxy {
                socket: PathBuf::from("/run/gvproxy/gvproxy.sock"),
            }),
            _ if s.starts_with("tap:") => Ok(NetworkMode::Tap {
                name: s.trim_start_matches("tap:").to_string(),
            }),
            _ => Err(format!("unknown network mode '{s}' (none|gvproxy|tap:<dev>)")),
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
            vcpus: default_vcpus(),
            ram_mib: default_ram(),
            attach_stdin: false,
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
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Sandbox {
    pub fn new(spec: SandboxSpec) -> Self {
        Self {
            id: SandboxId::new(),
            spec,
            state: SandboxState::Created,
            exit_code: None,
            pid: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            finished_at: None,
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
