//! Docker-compatible image reference parsing.
//!
//! Examples:
//!   `alpine`            -> registry-1.docker.io / library/alpine : latest
//!   `alpine:3.20`       -> registry-1.docker.io / library/alpine : 3.20
//!   `user/repo:tag`     -> registry-1.docker.io / user/repo : tag
//!   `ghcr.io/a/b:1`     -> ghcr.io / a/b : 1
//!   `localhost:5000/x`  -> localhost:5000 / x : latest
//!   `alpine@sha256:..`  -> digest reference

use crate::{img_err, ImgResult};

const DEFAULT_REGISTRY: &str = "registry-1.docker.io";
const DOCKER_HUB_ALIASES: [&str; 2] = ["docker.io", "index.docker.io"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    /// Registry host for API calls (docker.io normalized to registry-1.docker.io).
    pub registry: String,
    /// Repository path (library/ prefix applied for official docker images).
    pub repository: String,
    /// Tag or digest.
    pub reference: RefKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Tag(String),
    Digest(String),
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reference {
            RefKind::Tag(t) => write!(f, "{}/{}:{}", self.registry, self.repository, t),
            RefKind::Digest(d) => write!(f, "{}/{}@{}", self.registry, self.repository, d),
        }
    }
}

impl ImageReference {
    pub fn parse(input: &str) -> ImgResult<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(img_err("empty image reference"));
        }

        // Split digest.
        let (name_part, digest) = match input.split_once('@') {
            Some((n, d)) => (n, Some(d.to_string())),
            None => (input, None),
        };

        // Split registry from repository: first component is a registry if it
        // contains '.' or ':' or is "localhost".
        let (registry, rest) = match name_part.split_once('/') {
            Some((first, rest))
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                (normalize_registry(first), rest.to_string())
            }
            Some((_, _)) => (DEFAULT_REGISTRY.to_string(), name_part.to_string()),
            None => (DEFAULT_REGISTRY.to_string(), name_part.to_string()),
        };

        // Docker Hub official images get the library/ prefix.
        let repository = if registry == DEFAULT_REGISTRY && !rest.contains('/') {
            format!("library/{rest}")
        } else {
            rest
        };

        if repository.is_empty() {
            return Err(img_err(format!("invalid image reference '{input}'")));
        }

        // Tag: only from the part after the last '/' (so registry ports don't
        // confuse us) and only when no digest was given.
        let (repository, tag) = if digest.is_none() {
            match repository.rsplit_once(':') {
                Some((repo, tag)) if repo.contains('/') || !repo.is_empty() => {
                    (repo.to_string(), tag.to_string())
                }
                _ => (repository, "latest".to_string()),
            }
        } else {
            (repository, String::new())
        };

        let reference = match digest {
            Some(d) => RefKind::Digest(d),
            None => RefKind::Tag(tag),
        };

        Ok(Self {
            registry,
            repository,
            reference,
        })
    }

    /// The reference string used in registry URLs (tag or digest).
    pub fn ref_str(&self) -> &str {
        match &self.reference {
            RefKind::Tag(t) => t,
            RefKind::Digest(d) => d,
        }
    }

    /// Original-looking short form for display (docker.io hidden).
    pub fn familiar(&self) -> String {
        let repo = self
            .repository
            .strip_prefix("library/")
            .unwrap_or(&self.repository);
        let host = if self.registry == DEFAULT_REGISTRY {
            String::new()
        } else {
            format!("{}/", self.registry)
        };
        match &self.reference {
            RefKind::Tag(t) => format!("{host}{repo}:{t}"),
            RefKind::Digest(d) => format!("{host}{repo}@{d}"),
        }
    }

    /// Filesystem-safe key for the local store.
    pub fn store_key(&self) -> String {
        use sha2::{Digest as _, Sha256};
        let raw = self.to_string();
        let hash = hex::encode(Sha256::digest(raw.as_bytes()));
        let mut name: String = raw
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        name.truncate(48);
        format!("{name}-{}", &hash[..12])
    }
}

fn normalize_registry(host: &str) -> String {
    if DOCKER_HUB_ALIASES.contains(&host) {
        DEFAULT_REGISTRY.to_string()
    } else {
        host.to_string()
    }
}

impl std::str::FromStr for ImageReference {
    type Err = mvm_common::Error;
    fn from_str(s: &str) -> ImgResult<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_docker_reference() {
        let r = ImageReference::parse("alpine").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "library/alpine");
        assert_eq!(r.ref_str(), "latest");
        assert_eq!(r.familiar(), "alpine:latest");
    }

    #[test]
    fn parses_tag() {
        let r = ImageReference::parse("alpine:3.20").unwrap();
        assert_eq!(r.repository, "library/alpine");
        assert_eq!(r.ref_str(), "3.20");
    }

    #[test]
    fn parses_user_repo() {
        let r = ImageReference::parse("nginx/unit:latest").unwrap();
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "nginx/unit");
    }

    #[test]
    fn parses_other_registry() {
        let r = ImageReference::parse("ghcr.io/foo/bar:1.0").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "foo/bar");
        assert_eq!(r.ref_str(), "1.0");
    }

    #[test]
    fn parses_registry_with_port() {
        let r = ImageReference::parse("localhost:5000/myimg:dev").unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "myimg");
        assert_eq!(r.ref_str(), "dev");
    }

    #[test]
    fn parses_digest() {
        let r = ImageReference::parse("alpine@sha256:abc123").unwrap();
        assert_eq!(r.ref_str(), "sha256:abc123");
    }

    #[test]
    fn store_key_is_stable() {
        let a = ImageReference::parse("alpine:3.20").unwrap().store_key();
        let b = ImageReference::parse("alpine:3.20").unwrap().store_key();
        assert_eq!(a, b);
    }
}
