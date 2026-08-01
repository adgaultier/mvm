//! Request/response types for the mvm HTTP API (shared by server + clients).

use serde::{Deserialize, Serialize};

/// POST /api/v1/sandboxes/{id}/exec
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    pub workdir: Option<String>,
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
