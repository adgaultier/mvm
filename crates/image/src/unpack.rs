//! Layer unpacking with OCI whiteout handling.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use mvm_common::protocol::OwnershipEntry;

use crate::{img_err, ImgResult};

/// Accumulates tar-header ownership across layers. Rootless unpacks cannot
/// chown, so the real uid/gid/mode of every entry is recorded here and later
/// re-applied inside the guest (where root actually is root).
#[derive(Default)]
pub struct OwnershipManifest {
    /// path (relative, no leading ./) -> (uid, gid, mode)
    entries: BTreeMap<String, (u32, u32, u32)>,
}

impl OwnershipManifest {
    fn record(&mut self, path: &Path, uid: u32, gid: u32, mode: u32) {
        let key = normalize(path);
        if !key.is_empty() {
            self.entries.insert(key, (uid, gid, mode));
        }
    }

    /// A later layer deleted this path (whiteout): drop it and any children.
    fn remove_subtree(&mut self, path: &Path) {
        let key = normalize(path);
        let prefix = format!("{key}/");
        self.entries
            .retain(|k, _| k != &key && !k.starts_with(&prefix));
    }

    /// An opaque whiteout cleared everything under `dir` (children only).
    fn clear_children(&mut self, dir: &Path) {
        let key = normalize(dir);
        let prefix = if key.is_empty() { String::new() } else { format!("{key}/") };
        self.entries.retain(|k, _| !k.starts_with(&prefix) || k == &key);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Write as JSON-lines (one `OwnershipEntry` per line).
    pub fn save(&self, path: &Path) -> ImgResult<()> {
        use std::io::Write;
        let file = std::fs::File::create(path)?;
        let mut w = std::io::BufWriter::new(file);
        for (p, (u, g, m)) in &self.entries {
            let entry = OwnershipEntry { p: p.clone(), u: *u, g: *g, m: *m };
            let line = serde_json::to_string(&entry).map_err(|e| img_err(format!("manifest: {e}")))?;
            writeln!(w, "{line}")?;
        }
        w.flush()?;
        Ok(())
    }
}

/// Normalize a tar path to a clean relative key ("./usr//bin" -> "usr/bin").
fn normalize(path: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.components() {
        if let std::path::Component::Normal(c) = comp {
            if let Some(s) = c.to_str() {
                parts.push(s);
            }
        }
    }
    parts.join("/")
}

/// Unpack one layer blob (tar / tar+gzip / tar+zstd) onto `dest`,
/// applying whiteouts and recording ownership into `manifest`.
pub fn unpack_layer(
    blob: &[u8],
    media_type: &str,
    dest: &Path,
    manifest: &mut OwnershipManifest,
) -> ImgResult<()> {
    let reader: Box<dyn Read> = if media_type.ends_with("+gzip")
        || media_type.contains("tar.gzip")
        || is_gzip(blob)
    {
        Box::new(flate2::read::GzDecoder::new(blob))
    } else if media_type.ends_with("+zstd") || is_zstd(blob) {
        let dec = zstd::stream::read::Decoder::new(blob)
            .map_err(|e| img_err(format!("zstd decoder: {e}")))?;
        Box::new(dec)
    } else {
        Box::new(blob)
    };

    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    // Note: the tar crate only applies ownership when running as root,
    // which is exactly what we want (rootless unpacks skip chown).
    archive.set_unpack_xattrs(false);

    let entries = archive
        .entries()
        .map_err(|e| img_err(format!("tar entries: {e}")))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| img_err(format!("tar entry: {e}")))?;
        let path: PathBuf = entry
            .path()
            .map_err(|e| img_err(format!("tar path: {e}")))?
            .into_owned();

        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let parent: PathBuf = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();

        // OCI whiteouts.
        if name == ".wh..wh..opq" {
            // Opaque directory: remove all existing content of the parent dir.
            let dir = safe_join(dest, &parent)?;
            if dir.is_dir() {
                for child in std::fs::read_dir(&dir)? {
                    let child = child?;
                    remove_all(&child.path())?;
                }
            }
            manifest.clear_children(&parent);
            continue;
        }
        if let Some(rest) = name.strip_prefix(".wh.") {
            // Whiteout: delete the corresponding path.
            let removed = parent.join(rest);
            let target = safe_join(dest, &removed)?;
            remove_all(&target)?;
            manifest.remove_subtree(&removed);
            continue;
        }

        // Skip device nodes unless we're real (init-namespace) root: the
        // guest gets a proper /dev from devtmpfs at boot anyway, and
        // namespace-root cannot mknod.
        let header_type = entry.header().entry_type();
        if !mvm_common::is_init_ns_root()
            && (header_type == tar::EntryType::Char
                || header_type == tar::EntryType::Block
                || header_type == tar::EntryType::Fifo)
        {
            continue;
        }

        // A later OCI layer may replace an existing path with a hard link.
        // tar::Entry::unpack_in does not remove the old path first, so the
        // hard-link creation fails with EEXIST instead of applying the layer.
        if header_type == tar::EntryType::Link {
            remove_all(&safe_join(dest, &path)?)?;
        }

        let uid = entry.header().uid().unwrap_or(0) as u32;
        let gid = entry.header().gid().unwrap_or(0) as u32;
        let mode = entry.header().mode().unwrap_or(0o644);

        // unpack_in guards against path escapes; false => unsafe path skipped.
        let ok = entry
            .unpack_in(dest)
            .map_err(|e| {
                img_err(format!(
                    "unpack {} type={:?} link={:?}: {e:?}",
                    path.display(),
                    entry.header().entry_type(),
                    entry.link_name().ok().flatten()
                ))
            })?;
        if !ok {
            tracing::warn!("skipped unsafe tar path: {}", path.display());
            continue;
        }
        manifest.record(&path, uid, gid, mode);

        // As root — real or namespace-root with a mapped subid range —
        // apply the recorded owner directly. Namespace-root chowns land on
        // subuids, which virtiofs then presents back correctly.
        if is_root() {
            if let Ok(target) = safe_join(dest, &path) {
                apply_owner(&target, uid, gid, mode);
            }
        }
    }
    Ok(())
}

/// Best-effort lchown + setuid/setgid restoration (chown clears those bits).
fn apply_owner(path: &Path, uid: u32, gid: u32, mode: u32) {
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let rc = unsafe { libc::lchown(c_path.as_ptr(), uid, gid) };
    if rc == 0
        && mode & 0o6000 != 0
        && path.symlink_metadata().map(|m| !m.is_symlink()).unwrap_or(false)
    {
        unsafe { libc::chmod(c_path.as_ptr(), mode as libc::mode_t) };
    }
}

fn remove_all(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Join `p` onto `base`, refusing paths that escape the base.
fn safe_join(base: &Path, p: &Path) -> ImgResult<PathBuf> {
    let mut out = base.to_path_buf();
    for comp in p.components() {
        match comp {
            std::path::Component::Normal(c) => out.push(c),
            std::path::Component::CurDir => {}
            _ => return Err(img_err(format!("unsafe path in layer: {}", p.display()))),
        }
    }
    Ok(out)
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn is_zstd(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[..4] == [0x28, 0xb5, 0x2f, 0xfd]
}

fn is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    fn build_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        build_tar_owned(&entries.iter().map(|(n, d)| (*n, *d, 0, 0)).collect::<Vec<_>>())
    }

    fn build_tar_owned(entries: &[(&str, &[u8], u64, u64)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, uid, gid) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_uid(*uid);
            header.set_gid(*gid);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *data)
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn unpack(tar: &[u8], media: &str, dest: &Path) -> OwnershipManifest {
        let mut manifest = OwnershipManifest::default();
        unpack_layer(tar, media, dest, &mut manifest).unwrap();
        manifest
    }

    #[test]
    fn unpacks_plain_tar() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tar = build_tar(&[("hello.txt", b"world"), ("dir/inner.txt", b"inner")]);
        unpack(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp);
        assert_eq!(std::fs::read(tmp.join("hello.txt")).unwrap(), b"world");
        assert_eq!(std::fs::read(tmp.join("dir/inner.txt")).unwrap(), b"inner");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn applies_whiteout() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-wh-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("doomed.txt"), b"x").unwrap();
        let tar = build_tar(&[(".wh.doomed.txt", b"")]);
        unpack(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp);
        assert!(!tmp.join("doomed.txt").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn applies_opaque_dir() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-opq-{}", std::process::id()));
        let d = tmp.join("etc");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("old.conf"), b"x").unwrap();
        let tar = build_tar(&[("etc/.wh..wh..opq", b""), ("etc/new.conf", b"n")]);
        unpack(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp);
        assert!(!d.join("old.conf").exists());
        assert!(d.join("new.conf").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn unpacks_gzip() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-gz-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tar = build_tar(&[("gz.txt", b"gzdata")]);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar).unwrap();
        let gz = enc.finish().unwrap();
        unpack(&gz, "application/vnd.oci.image.layer.v1.tar+gzip", &tmp);
        assert_eq!(std::fs::read(tmp.join("gz.txt")).unwrap(), b"gzdata");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn replaces_existing_path_with_hard_link() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-link-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let layer1 = build_tar(&[("usr/bin/perl5.40.1", b"old")]);
        unpack(&layer1, "application/vnd.oci.image.layer.v1.tar", &tmp);

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_link(&mut header, "usr/bin/perl5.40.1", "usr/bin/perl")
            .unwrap();
        let layer2 = builder.into_inner().unwrap();
        std::fs::write(tmp.join("usr/bin/perl"), b"new").unwrap();
        unpack(&layer2, "application/vnd.oci.image.layer.v1.tar", &tmp);

        assert_eq!(std::fs::read(tmp.join("usr/bin/perl5.40.1")).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(tmp.join("usr/bin/perl")).unwrap().ino(),
            std::fs::metadata(tmp.join("usr/bin/perl5.40.1")).unwrap().ino()
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn manifest_records_ownership_and_honors_whiteouts() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-own-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Layer 1: root- and daemon-owned files.
        let l1 = build_tar_owned(&[
            ("bin/su", b"x" as &[u8], 0, 0),
            ("var/spool/mail", b"m", 8, 12),
            ("tmp/scratch", b"s", 0, 0),
        ]);
        let mut manifest = OwnershipManifest::default();
        unpack_layer(&l1, "application/vnd.oci.image.layer.v1.tar", &tmp, &mut manifest).unwrap();

        // Layer 2: whiteout for tmp/scratch, chown of mail spool.
        let l2 = build_tar_owned(&[("tmp/.wh.scratch", b"", 0, 0), ("var/spool/mail", b"m", 100, 100)]);
        unpack_layer(&l2, "application/vnd.oci.image.layer.v1.tar", &tmp, &mut manifest).unwrap();

        let out = tmp.join("ownership.jsonl");
        manifest.save(&out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let entries: Vec<OwnershipEntry> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        assert!(entries.iter().any(|e| e.p == "bin/su" && e.u == 0 && e.g == 0));
        assert!(entries.iter().any(|e| e.p == "var/spool/mail" && e.u == 100));
        assert!(!entries.iter().any(|e| e.p == "tmp/scratch"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
