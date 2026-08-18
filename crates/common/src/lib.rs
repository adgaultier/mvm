//! Shared types for the mvm microVM sandbox platform.

#[cfg(feature = "agent-api")]
pub mod agent_api;
pub mod api;
pub mod auth;
pub mod id;
pub mod names;
pub mod paths;
pub mod protocol;
pub mod spec;

#[cfg(feature = "agent-api")]
pub use auth::Principal;
pub use id::*;
pub use paths::*;
pub use spec::*;

/// Result alias used across mvm crates.
pub type Result<T> = std::result::Result<T, Error>;

/// Unified error type for the platform.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sandbox not found: {0}")]
    SandboxNotFound(String),

    #[error("image not found: {0}")]
    ImageNotFound(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("image error: {0}")]
    Image(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("{0}")]
    Other(String),
}
