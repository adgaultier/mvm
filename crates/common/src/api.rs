//! Request/response types for the mvm HTTP API (shared by server + clients).

use serde::{Deserialize, Serialize};

use crate::SandboxSpec;

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
    /// Run as this user instead of the workload's identity (`exec -u`).
    #[serde(default)]
    pub user: Option<String>,
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

/// POST /api/v1/images/load — query params (the body is the raw OCI-layout
/// `.tar`, which carries no name of its own, hence the required `name`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadQuery {
    /// Name/tag to store the image under (e.g. "myimg:latest").
    pub name: Option<String>,
}

/// POST /api/v1/sandboxes/{id}/clone — new sandbox from the source's spec.
/// `fork` carries the source's current disk into the clone (mvm clone --fork).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneRequest {
    /// The fully overridden spec the client wants; the source record is only
    /// referenced for its disk (when forking), never merged here.
    pub spec: SandboxSpec,
    #[serde(default)]
    pub fork: bool,
}

/// Agent API "delegate" params — ask the host to launch a child clone of the
/// calling sandbox, bounded by `timeout`. Not yet implemented: the handler
/// authenticates and authorizes, then reports that delegation is still in
/// progress. Gated behind the `agent-api` feature.
#[cfg(feature = "agent-api")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRequest {
    /// Seconds the child may run before it is stopped.
    pub timeout: u64,
    /// Command for the child sandbox (image/env/etc. inherit from the caller;
    /// mounts are supplied by the host policy, not the caller).
    pub command: Vec<String>,
}

/// Agent API request envelope: the guest's `mvm-agent-mcp` bridge dials the
/// host over vsock (`protocol::AGENT_API_VSOCK_PORT`) and sends exactly one
/// of these per connection, length-prefixed (`protocol::encode_frame`).
/// `token` is the VM-scoped bearer token (see `auth`), presented in the body
/// rather than a header since this isn't HTTP.
#[cfg(feature = "agent-api")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentApiRequest {
    pub method: String,
    pub token: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Agent API response envelope: exactly one per request, then the connection
/// closes.
#[cfg(feature = "agent-api")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentApiResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(feature = "agent-api")]
impl AgentApiResponse {
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.into()),
        }
    }
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
    /// Stream the *live* console byte-exact, terminal queries included.
    /// Only an interactive console session (`mvm attach`, `mvm run -it`)
    /// wants this: it owns the terminal and reads the reply. A plain reader
    /// (`mvm logs -f`) never answers, so a query would make its terminal
    /// reply into its own input buffer — hence filtered by default, the way
    /// the recorded backlog already is.
    #[serde(default)]
    pub raw: bool,
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
