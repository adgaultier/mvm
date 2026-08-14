//! Principals and VM-scoped bearer tokens for authenticating the restricted
//! Agent API (`/agent/v1`).
//!
//! A sandbox's VM is provisioned a cryptographically random token at boot;
//! only its SHA-256 hash is ever kept host-side, and only in the manager's
//! memory — never persisted to disk or serialized into an API response. The
//! token is opaque: it identifies *who* is calling (`Principal::Vm(vm_id)`);
//! what they may do is decided by the authorization layer, never by the token
//! itself.

use rand::RngCore;
use sha2::{Digest, Sha256};

#[cfg(feature = "agent-api")]
use crate::SandboxId;

/// Who is making an API request. Today only VMs authenticate (via the Agent
/// API); the privileged control-plane surface is deliberately left
/// unauthenticated for now and will gain a `Human` principal later.
///
/// Gated behind the `agent-api` feature: only the Agent API (and its manager
/// side) ever constructs or inspects a principal.
#[cfg(feature = "agent-api")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// A sandbox VM, authenticated by its VM-scoped bearer token.
    Vm(SandboxId),
}

#[cfg(feature = "agent-api")]
impl Principal {
    /// The sandbox this principal is authorized to act on, if any.
    pub fn vm_id(&self) -> Option<&SandboxId> {
        match self {
            Principal::Vm(id) => Some(id),
        }
    }
}

/// Number of random bytes in a VM token (32 bytes → 256 bits of entropy).
pub const TOKEN_LEN: usize = 32;

/// Generate a fresh cryptographically random VM token, returned as a hex
/// string (the bearer value presented over `Authorization: Bearer <token>`).
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_LEN];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// SHA-256 hash of a token, hex-encoded. This is the only form ever kept
/// host-side (in memory; never persisted or exposed over the API).
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Constant-time comparison of two byte strings of equal length.
///
/// The stored token hash is a key derived from a secret, so comparing it with
/// `==` (or a `HashMap` keyed on it) would leak the hash prefix through
/// timing. Tokens are unique 256-bit random values, so the manager scans the
/// (small) sandbox list and compares hashes with this fixed-time routine.
/// Only the Agent API's token verification uses it.
#[cfg(feature = "agent-api")]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_well_formed() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_LEN * 2); // hex
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_is_deterministic_and_never_the_token() {
        let token = generate_token();
        assert_eq!(hash_token(&token), hash_token(&token));
        assert_ne!(hash_token(&token), token);
        assert_eq!(hash_token(&token).len(), 64);
    }

    #[test]
    #[cfg(feature = "agent-api")]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
