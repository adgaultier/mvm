//! Rootless block-device storage driver.
//!
//! Builds a per-sandbox ext4 image from the image rootfs with
//! `mkfs.ext4 -d` (which needs no privileges) and boots the guest from it
//! via virtio-blk. Ownership/chown then has full POSIX semantics inside the
//! guest — unlike virtiofs, whose host-side server is stuck with the
//! daemon's credentials.

use mvm_common::{DataDir, Result, SandboxId};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{storage_err, PreparedRootfs, StorageDriver};

const DISK_FILE: &str = "disk.img";
const BOOTSTRAP_DIR: &str = "bootstrap";

/// Extra writable space added on top of the image rootfs size, MiB.
/// The image file is sparse, so slack costs almost nothing on disk.
const DEFAULT_SLACK_MIB: u64 = 1024;

pub struct Ext4Driver {
    data_dir: DataDir,
}

impl Ext4Driver {
    pub fn new(data_dir: DataDir) -> Self {
        Self { data_dir }
    }

    /// mkfs.ext4 with -d support is required (e2fsprogs >= 1.43).
    pub fn available() -> bool {
        Command::new("mkfs.ext4")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl StorageDriver for Ext4Driver {
    fn name(&self) -> &'static str {
        "ext4"
    }

    fn create(&self, id: &SandboxId, image_rootfs: &Path) -> Result<PreparedRootfs> {
        let sb_dir = self.data_dir.sandbox_dir(id);
        std::fs::create_dir_all(&sb_dir)?;

        // The virtiofs root the guest actually boots is a tiny bootstrap dir
        // holding only the agent (+ ownership manifest); the agent pivots
        // onto the disk. Recreate it fresh each start.
        let bootstrap = sb_dir.join(BOOTSTRAP_DIR);
        if bootstrap.exists() {
            std::fs::remove_dir_all(&bootstrap)?;
        }
        std::fs::create_dir_all(&bootstrap)?;

        // Reuse an existing disk across stop/start (docker-like persistence).
        // Interrupted builds can't leak: the image is built as a tmp file and
        // renamed into place only when mkfs succeeds.
        let disk = sb_dir.join(DISK_FILE);
        if !disk.exists() {
            build_disk(image_rootfs, &disk)?;
        }

        Ok(PreparedRootfs {
            rootfs: bootstrap,
            root_disk: Some(disk),
        })
    }

    fn destroy(&self, id: &SandboxId) -> Result<()> {
        let dir = self.data_dir.sandbox_dir(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// Build an ext4 image at `disk` populated from `rootfs`.
fn build_disk(rootfs: &Path, disk: &Path) -> Result<PathBuf> {
    let slack_mib = std::env::var("MVM_DISK_SLACK_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SLACK_MIB);
    let size = disk_size_bytes(dir_size(rootfs), slack_mib);

    let tmp = disk.with_extension("img.tmp");
    let _ = std::fs::remove_file(&tmp);
    let file = std::fs::File::create(&tmp)?;
    file.set_len(size)?;
    drop(file);

    let out = Command::new("mkfs.ext4")
        .arg("-F")
        .arg("-q")
        .arg("-E")
        .arg("lazy_itable_init=1")
        .arg("-d")
        .arg(rootfs)
        .arg(&tmp)
        .output()
        .map_err(|e| storage_err(format!("spawning mkfs.ext4: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(storage_err(format!(
            "mkfs.ext4 failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    std::fs::rename(&tmp, disk)?;
    Ok(disk.to_path_buf())
}

/// Total ext4 image size: rootfs contents + slack, floored so mkfs always
/// has room for its own metadata.
fn disk_size_bytes(content_bytes: u64, slack_mib: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    let content_mib = content_bytes / MIB + 1;
    // ~5% metadata overhead estimate, 64 MiB minimum total.
    let total = (content_mib + content_mib / 20 + slack_mib).max(64);
    total * MIB
}

/// Apparent size of a directory tree (lstat, no symlink following).
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_have_floor_and_slack() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(disk_size_bytes(0, 0), 64 * MIB);
        let s = disk_size_bytes(100 * MIB, 1024);
        assert!(s >= 1124 * MIB && s < 1200 * MIB, "got {s}");
    }

    #[test]
    fn builds_ext4_image_from_dir() {
        if !Ext4Driver::available() {
            eprintln!("skipping: mkfs.ext4 not available");
            return;
        }
        let base = std::env::temp_dir().join(format!("mvm-ext4-{}", std::process::id()));
        let rootfs = base.join("rootfs");
        std::fs::create_dir_all(rootfs.join("etc")).unwrap();
        std::fs::write(rootfs.join("etc/hostname"), b"sbx\n").unwrap();
        std::os::unix::fs::symlink("etc/hostname", rootfs.join("hn")).unwrap();

        let disk = base.join("disk.img");
        build_disk(&rootfs, &disk).unwrap();

        // ext4 superblock magic 0xEF53 at offset 0x438.
        let img = std::fs::read(&disk).unwrap();
        assert_eq!(&img[0x438..0x43a], &[0x53, 0xef]);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
