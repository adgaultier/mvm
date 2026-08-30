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

/// Guest hostname: the sanitized name, or the plain id when unnamed.
pub fn sandbox_hostname(name: Option<&str>, id: &str) -> String {
    match name.map(sanitize_hostname).filter(|s| !s.is_empty()) {
        Some(sanitized) => sanitized,
        None => id.to_string(),
    }
}

/// Hostname-safe form: lowercase alnum and dashes.
fn sanitize_hostname(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unnamed_sandbox_uses_id() {
        assert_eq!(sandbox_hostname(None, "a1b2c3d4e5f6"), "a1b2c3d4e5f6");
    }

    #[test]
    fn named_sandbox_gets_name_only() {
        assert_eq!(sandbox_hostname(Some("web"), "a1b2c3d4e5f6"), "web");
    }

    #[test]
    fn name_is_sanitized_to_hostname_safe_ldh() {
        assert_eq!(
            sandbox_hostname(Some(" My_Agent./v2 "), "a1b2c3d4e5f6"),
            "my-agent-v2"
        );
        assert_eq!(sanitize_hostname("--trailing--"), "trailing");
        assert_eq!(sanitize_hostname("UPPER"), "upper");
    }

    #[test]
    fn unsalvageable_name_falls_back_to_id() {
        assert_eq!(
            sandbox_hostname(Some("---"), "a1b2c3d4e5f6"),
            "a1b2c3d4e5f6"
        );
    }
}
