//! Request/response types for the mvm HTTP API (shared by server + clients).

use serde::{Deserialize, Serialize};

/// POST /api/v1/sandboxes/{id}/exec
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    pub workdir: Option<String>,
    /// Allocate a pseudo-terminal for the process.
    #[serde(default)]
    pub tty: bool,
    /// Initial terminal size (0 = unspecified).
    #[serde(default)]
    pub cols: u16,
    #[serde(default)]
    pub rows: u16,
}

/// POST /api/v1/sandboxes/{id}/exec/{session}/resize
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeRequest {
    pub cols: u16,
    pub rows: u16,
}

/// POST /api/v1/sandboxes/{id}/resize — change the VM's CPU/RAM allocation.
/// Omitted fields keep their current value. A microVM cannot be resized while
/// it runs, so this rewrites the spec and the next boot picks it up.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxResizeRequest {
    pub vcpus: Option<u8>,
    pub ram_mib: Option<u32>,
}

/// POST /api/v1/images/pull
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub reference: String,
}

/// Uniform error body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Query for the logs endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub follow: bool,
    /// Cap the backlog to this many trailing lines (absent = the whole log).
    #[serde(default)]
    pub tail: Option<usize>,
}

/// Query for the exec stdin endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StdinQuery {
    /// Close the session's stdin instead of writing data.
    #[serde(default)]
    pub eof: bool,
}

/// Daemon info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoResponse {
    pub version: String,
    pub storage_driver: String,
}
