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

/// Security profile for a sandbox.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityProfile {
    /// Docker-style compatibility: root workloads keep the usual syscall
    /// surface (the always-on raw-socket ban still applies).
    #[default]
    Default,
    /// Hostile-workload hardening: the guestd installs an additional
    /// seccomp filter in the workload's spawn path denying the high-risk
    /// syscalls (`bpf`, `keyctl`, `perf_event_open`, `userfaultfd`,
    /// `io_uring`). Fail-closed by design — this is for untrusted code.
    Strict,
}

impl std::str::FromStr for SecurityProfile {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "default" => Ok(SecurityProfile::Default),
            "strict" => Ok(SecurityProfile::Strict),
            _ => Err(format!(
                "unknown security profile '{s}' (default|strict)"
            )),
        }
    }
}

impl std::fmt::Display for SecurityProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SecurityProfile::Default => "default",
            SecurityProfile::Strict => "strict",
        })
    }
}

/// User-provided specification for a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// Optional human-friendly name.
    pub name: Option<String>,
    /// Image reference (as pulled, e.g. "alpine:latest").
    pub image: String,
    /// Command + args to run as the workload (PID 1 child of the guestd).
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
    /// Security profile (default|strict). Strict installs an additional
    /// guest-side seccomp filter denying high-risk syscalls in the workload's
    /// spawn path; intended for hostile workloads.
    #[serde(default)]
    pub security: SecurityProfile,
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
            security: SecurityProfile::Default,
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

/// One timestamped event in a sandbox's unified timeline. Everything the TUI
/// renders — lifecycle ops with their phases, and discrete events (guestd
/// connecting, agent ready) — is recorded as the same type, sorted by `at`.
///
/// Conventions for `event`:
/// - `create`, `start`, `stop`: lifecycle operation begins.
/// - `<phase>_start` / `<phase>_stop` (e.g. `rootfs_start`, `rootfs_stop`):
///   the boundaries of a timed phase within an op. The bar segment between
///   them is colored by the phase name.
/// - `agent_ready`, etc.: point-in-time signals with no duration; rendered as
///   markers on the bar at their timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    /// What happened (see type-level doc for naming conventions).
    pub event: String,
    /// When it happened.
    pub at: chrono::DateTime<chrono::Utc>,
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
    /// When the guestd signalled `Ready` (infrastructure boot complete:
    /// seccomp, mounts, network, workload spawned, vsock control channel
    /// up). Set from `GuestdEvent::Ready`; cleared on stop/exit. Applies to
    /// every sandbox, not just agent-backed ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the workload declared itself ready (steady state reached: boot
    /// and runtime init complete). Set by the Agent API `ready` method,
    /// called by the guest's `mvm-agent-mcp` bridge; cleared on stop/exit.
    /// Stays `None` for sandboxes that never call it (the control plane
    /// can't infer application readiness for an arbitrary workload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last size the console workload pty was resized to (cols, rows). The
    /// spec's `tty_size` is the create-time initial; this tracks live
    /// `/console/resize` calls and is None until one arrives.
    #[serde(default)]
    pub console_size: Option<(u16, u16)>,
    /// Per-lifecycle timeline of timestamped events. Each inner Vec is one
    /// lifecycle's events (start, stop) in chronological order — op
    /// boundaries (`<op>_start`/`<op>_stop`), phase boundaries
    /// (`<phase>_start`/`<phase>_stop`), and point-in-time signals
    /// (`agent_ready`). The TUI renders one bar per lifecycle from the
    /// timestamps directly. Bounded by the manager.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<Vec<TimelineEvent>>,
    /// Lineage: the agent that delegated this sandbox into existence
    /// (`Manager::delegate`). `None` for root sandboxes created by a user.
    /// Persisted so lineage survives daemon restarts.
    #[cfg(feature = "agent-api")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SandboxId>,
    /// Display-only TTL: set when this sandbox was delegated with a timeout;
    /// the deadline at which it expires. Enforcement (stop+rm of the agent
    /// and its children, per `doc/agentic/notes.md`) is a follow-up — graph
    /// views render a countdown from this for now.
    #[cfg(feature = "agent-api")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_deadline: Option<chrono::DateTime<chrono::Utc>>,
    /// Bounded in-memory history of notifications delivered to this agent
    /// (newest last, capped by the manager). Volatile — never serialized —
    /// feeding `mvm-flow`'s edge labels.
    #[cfg(feature = "agent-api")]
    #[serde(skip)]
    pub recent_notifications: Vec<crate::agent_api::Notification>,
    /// Shell command template the control plane runs with `mvm exec` to
    /// deliver async notifications to this agent (the spec's `async_cmd`;
    /// `<MSG>` is the placeholder for the serialized `Notification` JSON).
    /// Registered by the agent itself over the Agent API.
    #[cfg(feature = "agent-api")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_command: Option<String>,
    /// Notifications queued for this agent but not yet delivered (a delegated
    /// child receives its Daddy task this way). Flushed through
    /// `notification_command` once the agent declares `ready` and has
    /// registered the command — persisted so a daemon restart cannot lose a
    /// delegation.
    #[cfg(feature = "agent-api")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notifications: Vec<crate::agent_api::Notification>,
    /// SHA-256 hash of the sandbox's VM-scoped bearer token, held only while
    /// the VM is running. This is authentication material internal to the
    /// manager: `#[serde(skip)]` keeps it out of every API response and out
    /// of `sandboxes.json` (the plaintext token exists only transiently in
    /// the shim's process environment and in the guest). Regenerated on every
    /// start; cleared the moment the sandbox stops or exits.
    #[serde(skip)]
    pub guest_token_hash: Option<String>,
    /// When the current token was minted (its start time). Manager-internal
    /// bookkeeping, like the hash it accompanies — never serialized.
    #[serde(skip)]
    pub guest_token_created_at: Option<chrono::DateTime<chrono::Utc>>,
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
            booted_at: None,
            ready_at: None,
            finished_at: None,
            console_size: None,
            timeline: Vec::new(),
            #[cfg(feature = "agent-api")]
            parent: None,
            #[cfg(feature = "agent-api")]
            ttl_deadline: None,
            #[cfg(feature = "agent-api")]
            recent_notifications: Vec::new(),
            #[cfg(feature = "agent-api")]
            notification_command: None,
            #[cfg(feature = "agent-api")]
            pending_notifications: Vec::new(),
            guest_token_hash: None,
            guest_token_created_at: None,
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
