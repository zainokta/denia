use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use tar::Archive;

use super::{LayerBlob, LayerCompression, OciError};

pub struct TarRootfsUnpacker;

impl TarRootfsUnpacker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TarRootfsUnpacker {
    fn default() -> Self {
        Self::new()
    }
}

impl super::OciRootfsUnpacker for TarRootfsUnpacker {
    fn unpack(&self, layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
        fs::create_dir_all(rootfs_dir)?;
        for layer in layers {
            apply_layer(layer, rootfs_dir)?;
        }
        Ok(())
    }
}

fn apply_layer(layer: &LayerBlob, rootfs_dir: &Path) -> Result<(), OciError> {
    const MAX_UNCOMPRESSED_BYTES: u64 = 10u64 * 1024 * 1024 * 1024;
    const MAX_SINGLE_FILE_BYTES: u64 = 2u64 * 1024 * 1024 * 1024;
    const MAX_FILE_COUNT: u64 = 1_000_000;

    let file = std::fs::File::open(&layer.path)?;
    let buf = std::io::BufReader::new(file);
    let reader: Box<dyn Read> = match layer.compression {
        LayerCompression::Gzip => Box::new(GzDecoder::new(buf)),
        LayerCompression::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(buf)
                .map_err(|e| OciError::Io(std::io::Error::other(e)))?;
            Box::new(decoder)
        }
        LayerCompression::None => Box::new(buf),
    };

    let mut archive = Archive::new(reader);
    let mut pending_whiteouts: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut file_count: u64 = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.to_path_buf();

        let safe_path = safe_join(rootfs_dir, &entry_path)?;

        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == ".wh..wh..opq" {
            if let Some(parent) = safe_path.parent()
                && parent.starts_with(rootfs_dir)
                && parent.exists()
            {
                for child in fs::read_dir(parent)? {
                    let child = child?;
                    let child_path = child.path();
                    if child_path != safe_path {
                        if child_path.is_dir() {
                            let _ = fs::remove_dir_all(&child_path);
                        } else {
                            let _ = fs::remove_file(&child_path);
                        }
                    }
                }
            }
            continue;
        }

        if let Some(stripped) = file_name.strip_prefix(".wh.") {
            if let Some(parent) = safe_path.parent() {
                let target = parent.join(stripped);
                if target.starts_with(rootfs_dir) && target.exists() {
                    pending_whiteouts.push((target, safe_path.clone()));
                }
            }
            continue;
        }

        let is_symlink = entry.header().entry_type().is_symlink();
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&safe_path)?;
        } else if is_symlink {
            let target = entry
                .link_name()?
                .ok_or_else(|| OciError::Io(std::io::Error::other("symlink without target")))?;
            validate_symlink_target(&target, rootfs_dir)?;
            let _ = fs::remove_file(&safe_path);
            std::os::unix::fs::symlink(&target, &safe_path)?;
        } else {
            let entry_size = entry.header().entry_size()?;
            if entry_size > MAX_SINGLE_FILE_BYTES {
                return Err(OciError::Io(std::io::Error::other(format!(
                    "file exceeds per-file size limit ({} > {}): {}",
                    entry_size,
                    MAX_SINGLE_FILE_BYTES,
                    entry_path.display()
                ))));
            }
            file_count += 1;
            if file_count > MAX_FILE_COUNT {
                return Err(OciError::Io(std::io::Error::other(format!(
                    "layer exceeds file count limit ({} > {})",
                    file_count, MAX_FILE_COUNT
                ))));
            }
            if let Some(parent) = safe_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&safe_path)?;
            let written = std::io::copy(&mut entry, &mut file)?;
            total_bytes += written;
            if total_bytes > MAX_UNCOMPRESSED_BYTES {
                return Err(OciError::Io(std::io::Error::other(format!(
                    "layer exceeds total size limit ({} > {})",
                    total_bytes, MAX_UNCOMPRESSED_BYTES
                ))));
            }
        }

        if !is_symlink {
            let mode = entry.header().mode()?;
            if mode != 0 {
                let _ = fs::set_permissions(&safe_path, fs::Permissions::from_mode(mode));
            }
        }
    }

    for (target, _) in pending_whiteouts {
        if target.starts_with(rootfs_dir) && target.exists() {
            if target.is_dir() {
                let _ = fs::remove_dir_all(&target);
            } else {
                let _ = fs::remove_file(&target);
            }
        }
    }

    Ok(())
}

fn safe_join(root: &Path, entry: &Path) -> Result<PathBuf, OciError> {
    if entry.is_absolute() {
        return Err(OciError::UnsafePath(format!(
            "absolute path rejected: {}",
            entry.display()
        )));
    }

    for component in entry.components() {
        if let std::path::Component::ParentDir = component {
            return Err(OciError::UnsafePath(format!(
                "parent dir component rejected: {}",
                entry.display()
            )));
        }
    }

    let joined = root.join(entry);

    let canonical = joined.canonicalize().unwrap_or_else(|_| joined.clone());
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    if !canonical.starts_with(&root_canonical) {
        return Err(OciError::UnsafePath(format!(
            "path traversal rejected: {}",
            entry.display()
        )));
    }

    Ok(joined)
}

fn validate_symlink_target(target: &Path, rootfs_dir: &Path) -> Result<(), OciError> {
    if target.is_absolute() {
        return Err(OciError::UnsafePath(format!(
            "symlink target is absolute: {}",
            target.display()
        )));
    }
    for component in target.components() {
        if let std::path::Component::ParentDir = component {
            return Err(OciError::UnsafePath(format!(
                "symlink target contains parent dir: {}",
                target.display()
            )));
        }
    }
    let joined = rootfs_dir.join(target);
    let root_canonical = rootfs_dir
        .canonicalize()
        .unwrap_or_else(|_| rootfs_dir.to_path_buf());
    if let Ok(canonical) = joined.canonicalize()
        && !canonical.starts_with(&root_canonical)
    {
        return Err(OciError::UnsafePath(format!(
            "symlink target escapes rootfs: {}",
            target.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{LayerBlob, OciRootfsUnpacker};
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tar::Builder;

    fn gz_layer(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> LayerBlob {
        let mut tar = Builder::new(Vec::new());
        for (path, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, *data).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, enc.finish().unwrap()).unwrap();
        LayerBlob {
            digest: "sha256:test".into(),
            compression: LayerCompression::Gzip,
            path,
        }
    }

    #[test]
    fn single_gzip_layer_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let layer = gz_layer(dir.path(), "l0.tar.gz", &[("hello.txt", b"world")]);
        let unpacker = TarRootfsUnpacker::new();
        unpacker.unpack(&[layer], &rootfs).unwrap();
        let content = fs::read_to_string(rootfs.join("hello.txt")).unwrap();
        assert_eq!(content, "world");
    }

    #[test]
    fn second_layer_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("a.txt", b"v1")]);
        let l2 = gz_layer(dir.path(), "l2.tar.gz", &[("a.txt", b"v2")]);
        TarRootfsUnpacker::new().unpack(&[l1, l2], &rootfs).unwrap();
        assert_eq!(fs::read_to_string(rootfs.join("a.txt")).unwrap(), "v2");
    }

    #[test]
    fn whiteout_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("foo", b"x")]);
        let l2 = gz_layer(dir.path(), "l2.tar.gz", &[(".wh.foo", b"")]);
        TarRootfsUnpacker::new().unpack(&[l1, l2], &rootfs).unwrap();
        assert!(!rootfs.join("foo").exists());
    }

    #[test]
    fn opaque_dir_clears_prior() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("d/old.txt", b"x")]);
        let l2 = gz_layer(
            dir.path(),
            "l2.tar.gz",
            &[("d/.wh..wh..opq", b""), ("d/new.txt", b"y")],
        );
        TarRootfsUnpacker::new().unpack(&[l1, l2], &rootfs).unwrap();
        assert!(!rootfs.join("d/old.txt").exists());
        assert_eq!(fs::read_to_string(rootfs.join("d/new.txt")).unwrap(), "y");
    }

    #[test]
    fn rejects_parent_traversal() {
        let rootfs = Path::new("/tmp/test-rootfs");
        let result = safe_join(rootfs, Path::new("../escape"));
        assert!(matches!(result, Err(OciError::UnsafePath(_))));
    }

    #[test]
    fn rejects_absolute_path() {
        let rootfs = Path::new("/tmp/test-rootfs");
        let result = safe_join(rootfs, Path::new("/etc/passwd"));
        assert!(matches!(result, Err(OciError::UnsafePath(_))));
    }

    #[test]
    fn safe_join_allows_normal_path() {
        let rootfs = Path::new("/tmp/test-rootfs");
        let result = safe_join(rootfs, Path::new("usr/bin/app")).unwrap();
        assert_eq!(result, Path::new("/tmp/test-rootfs/usr/bin/app"));
    }
}
