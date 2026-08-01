use serde::{Deserialize, Serialize};
use std::fmt;

/// Unique identifier for a sandbox (short docker-style hex id).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SandboxId(String);

impl SandboxId {
    /// Generate a new random id (12 hex chars, like short docker ids).
    pub fn new() -> Self {
        let uuid = uuid::Uuid::new_v4().simple().to_string();
        Self(uuid[..12].to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SandboxId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SandboxId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SandboxId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}
