use std::path::PathBuf;

/// Filesystem layout for all mvm state.
///
/// Rootless by default: ~/.local/share/mvm
/// As root: /var/lib/mvm (unless MVM_DATA_DIR overrides).
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Resolve the data directory using (in priority order):
    /// $MVM_DATA_DIR, root -> /var/lib/mvm, else ~/.local/share/mvm
    pub fn resolve() -> std::io::Result<Self> {
        if let Ok(dir) = std::env::var("MVM_DATA_DIR") {
            return Ok(Self { root: PathBuf::from(dir) });
        }
        let root = if is_root() {
            PathBuf::from("/var/lib/mvm")
        } else {
            let data = dirs::data_dir()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data dir"))?;
            data.join("mvm")
        };
        Ok(Self { root })
    }

    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Ensure the directory tree exists.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.images_dir())?;
        std::fs::create_dir_all(self.sandboxes_dir())?;
        Ok(())
    }

    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    pub fn sandboxes_dir(&self) -> PathBuf {
        self.root.join("sandboxes")
    }

    /// Directory holding everything related to one unpacked image.
    pub fn image_dir(&self, key: &str) -> PathBuf {
        self.images_dir().join(key)
    }

    pub fn sandbox_dir(&self, id: &crate::SandboxId) -> PathBuf {
        self.sandboxes_dir().join(id.as_str())
    }

    /// Path to the daemon's auth-free local HTTP endpoint config file.
    pub fn daemon_pidfile(&self) -> PathBuf {
        self.root.join("daemon.pid")
    }
}

/// Path to the guest agent binary, resolved at runtime.
/// Priority: $MVM_AGENT_PATH, then alongside the current executable.
pub fn agent_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MVM_AGENT_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("mvm-agent");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // Last resort: PATH lookup.
    which("mvm-agent")
}

/// Minimal PATH lookup (avoids pulling in the `which` crate).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn is_root() -> bool {
    unsafe { libc_geteuid() == 0 }
}

// Tiny direct syscall wrapper to avoid a libc dependency in common.
unsafe fn libc_geteuid() -> u32 {
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        1000
    }
}
