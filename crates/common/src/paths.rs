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

}

/// Path to the guest agent binary, resolved at runtime.
///
/// Priority: $MVM_AGENT_PATH (always honored), then — skipping any
/// dynamically-linked candidate, which could not exec inside guests whose
/// libc we don't control (musl images lack the glibc loader) — alongside
/// the current executable, the musl target dir of a dev tree, and PATH.
pub fn agent_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MVM_AGENT_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            if elf_has_interp(&p) {
                eprintln!(
                    "mvm: warning: MVM_AGENT_PATH agent {} is dynamically linked; \
                     it will fail to exec in guests without a matching libc",
                    p.display()
                );
            }
            return Some(p);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("mvm-agent"));
            // Dev tree: exe in target/<profile>/, static agent in
            // target/x86_64-unknown-linux-musl/release/.
            if let Some(target) = dir.parent() {
                candidates.push(
                    target
                        .join("x86_64-unknown-linux-musl")
                        .join("release")
                        .join("mvm-agent"),
                );
            }
        }
    }
    if let Some(p) = which("mvm-agent") {
        candidates.push(p);
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if elf_has_interp(&candidate) {
            eprintln!(
                "mvm: skipping dynamically-linked agent candidate {} \
                 (build the static one: cargo build --release -p mvm-agent \
                 --target x86_64-unknown-linux-musl)",
                candidate.display()
            );
            continue;
        }
        return Some(candidate);
    }
    None
}

/// True if the ELF at `path` requests an interpreter (PT_INTERP), i.e. is
/// dynamically linked. Unreadable/non-ELF files count as static so exotic
/// setups aren't rejected; the guest will surface a real error if any.
fn elf_has_interp(path: &std::path::Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    const PT_INTERP: u32 = 3;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut ehdr = [0u8; 64];
    if f.read_exact(&mut ehdr).is_err() || &ehdr[..4] != b"\x7fELF" || ehdr[4] != 2 {
        return false; // not ELF64
    }
    let phoff = u64::from_le_bytes(ehdr[0x20..0x28].try_into().unwrap());
    let phentsize = u16::from_le_bytes(ehdr[0x36..0x38].try_into().unwrap()) as u64;
    let phnum = u16::from_le_bytes(ehdr[0x38..0x3a].try_into().unwrap()) as u64;
    if phentsize < 4 || phnum == 0 || phnum > 512 {
        return false;
    }
    let mut ptype = [0u8; 4];
    for i in 0..phnum {
        if f.seek(SeekFrom::Start(phoff + i * phentsize)).is_err()
            || f.read_exact(&mut ptype).is_err()
        {
            return false;
        }
        if u32::from_le_bytes(ptype) == PT_INTERP {
            return true;
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses the workspace's own build artifacts when present: the host-gnu
    /// agent must register as dynamic, the musl one as static.
    #[test]
    fn classifies_agent_binaries() {
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let dynamic = target.join("debug/mvm-agent");
        if dynamic.is_file() {
            assert!(elf_has_interp(&dynamic), "gnu agent should be dynamic");
        }
        let static_ = target.join("x86_64-unknown-linux-musl/release/mvm-agent");
        if static_.is_file() {
            assert!(!elf_has_interp(&static_), "musl agent should be static");
        }
    }
}

pub fn is_root() -> bool {
    unsafe { libc_geteuid() == 0 }
}

/// True only for root in the *initial* user namespace. Namespace-root
/// (rootless userns mode) passes `is_root` but lacks privileges the init
/// namespace grants, e.g. mknod of device nodes.
pub fn is_init_ns_root() -> bool {
    is_root()
        && std::fs::read_to_string("/proc/self/uid_map")
            .map(|m| {
                let fields: Vec<&str> = m.split_whitespace().collect();
                fields == ["0", "0", "4294967295"]
            })
            .unwrap_or(false)
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
