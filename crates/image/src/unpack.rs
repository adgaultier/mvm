//! Layer unpacking with OCI whiteout handling.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{img_err, ImgResult};

/// Unpack one layer blob (tar / tar+gzip / tar+zstd) onto `dest`,
/// applying whiteouts.
pub fn unpack_layer(blob: &[u8], media_type: &str, dest: &Path) -> ImgResult<()> {
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
            continue;
        }
        if let Some(rest) = name.strip_prefix(".wh.") {
            // Whiteout: delete the corresponding path.
            let target = safe_join(dest, &parent.join(rest))?;
            remove_all(&target)?;
            continue;
        }

        // Skip device nodes when unprivileged: the guest gets a proper
        // /dev from devtmpfs at boot anyway.
        let header_type = entry.header().entry_type();
        if !is_root()
            && (header_type == tar::EntryType::Char
                || header_type == tar::EntryType::Block
                || header_type == tar::EntryType::Fifo)
        {
            continue;
        }

        // unpack_in guards against path escapes; false => unsafe path skipped.
        let ok = entry
            .unpack_in(dest)
            .map_err(|e| img_err(format!("unpack {}: {e}", path.display())))?;
        if !ok {
            tracing::warn!("skipped unsafe tar path: {}", path.display());
        }
    }
    Ok(())
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

    fn build_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *data)
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn unpacks_plain_tar() {
        let tmp = std::env::temp_dir().join(format!("mvm-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tar = build_tar(&[("hello.txt", b"world"), ("dir/inner.txt", b"inner")]);
        unpack_layer(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp).unwrap();
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
        unpack_layer(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp).unwrap();
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
        unpack_layer(&tar, "application/vnd.oci.image.layer.v1.tar", &tmp).unwrap();
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
        unpack_layer(&gz, "application/vnd.oci.image.layer.v1.tar+gzip", &tmp).unwrap();
        assert_eq!(std::fs::read(tmp.join("gz.txt")).unwrap(), b"gzdata");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
