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
                ensure_existing_dir_no_symlink(rootfs_dir, parent)?;
                for child in fs::read_dir(parent)? {
                    let child = child?;
                    let child_path = child.path();
                    if child_path != safe_path {
                        let meta = fs::symlink_metadata(&child_path)?;
                        if meta.is_dir() {
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
                if parent.exists() {
                    ensure_existing_dir_no_symlink(rootfs_dir, parent)?;
                }
                let target = parent.join(stripped);
                if target.starts_with(rootfs_dir) && fs::symlink_metadata(&target).is_ok() {
                    pending_whiteouts.push((target, safe_path.clone()));
                }
            }
            continue;
        }

        // Count EVERY materialized entry (dir, symlink, hardlink, regular
        // file) toward the inode-bomb guard. Counting only regular files let
        // an archive of hundreds of millions of cheap-on-the-wire directories
        // or symlinks exhaust inodes / fill the parent dir while staying under
        // the limit. Whiteouts are handled above and `continue` before here.
        file_count += 1;
        if file_count > MAX_FILE_COUNT {
            return Err(OciError::Io(std::io::Error::other(format!(
                "layer exceeds entry count limit ({} > {})",
                file_count, MAX_FILE_COUNT
            ))));
        }

        let is_symlink = entry.header().entry_type().is_symlink();
        if entry.header().entry_type().is_dir() {
            create_dir_all_no_symlink(rootfs_dir, &safe_path)?;
        } else if is_symlink {
            let target = entry
                .link_name()?
                .ok_or_else(|| OciError::Io(std::io::Error::other("symlink without target")))?;
            validate_symlink_target(&target, rootfs_dir)?;
            if let Some(parent) = safe_path.parent() {
                create_dir_all_no_symlink(rootfs_dir, parent)?;
            }
            let _ = fs::remove_file(&safe_path);
            // A prior layer may have created a real directory at this path that
            // the current layer redefines as a symlink (e.g. the `/lib ->
            // usr/lib` usrmerge in Debian-based images). `remove_file` cannot
            // remove a directory, so clear it explicitly; otherwise `symlink`
            // fails EEXIST. `symlink_metadata` does not follow links, so a
            // symlink already at this path reports `is_dir() == false` and is
            // left for `remove_file` above — only a genuine directory is razed.
            if let Ok(meta) = fs::symlink_metadata(&safe_path)
                && meta.is_dir()
            {
                fs::remove_dir_all(&safe_path)?;
            }
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
            if let Some(parent) = safe_path.parent() {
                create_dir_all_no_symlink(rootfs_dir, parent)?;
            }
            // Unlink any prior file/symlink at this path so File::create
            // cannot follow a stale symlink (from an earlier layer) out of
            // rootfs and overwrite a host file.
            if let Ok(meta) = fs::symlink_metadata(&safe_path)
                && !meta.is_dir()
            {
                let _ = fs::remove_file(&safe_path);
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
                let safe_mode = mode & 0o0777;
                let _ = fs::set_permissions(&safe_path, fs::Permissions::from_mode(safe_mode));
            }
        }
    }

    for (target, _) in pending_whiteouts {
        if target.starts_with(rootfs_dir)
            && let Ok(meta) = fs::symlink_metadata(&target)
        {
            if meta.is_dir() {
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

    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    // Tar archives commonly include `./` or `.` as the root marker — these
    // resolve to rootfs itself. Accept them; the caller treats them as a
    // no-op directory.
    let joined_canonical = joined.canonicalize().unwrap_or_else(|_| joined.clone());
    if joined_canonical == root_canonical {
        return Ok(joined);
    }

    // Only require the PARENT directory to resolve under rootfs. The leaf may
    // legitimately be a stale symlink from a prior layer (e.g.
    // `etc/alternatives/pager -> /usr/bin/less`) that this entry is about to
    // overwrite — canonicalizing the leaf would follow that symlink to the
    // host path and falsely report a traversal. Workload isolation comes
    // from pivot_root + mount namespace at runtime; the extractor's job is
    // to keep parent directories under rootfs and unlink stale targets
    // before writing (caller does this in `apply_layer`).
    let parent = joined.parent().unwrap_or(root);
    let parent_canonical = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());

    if !parent_canonical.starts_with(&root_canonical) {
        return Err(OciError::UnsafePath(format!(
            "path traversal rejected: {}",
            entry.display()
        )));
    }

    Ok(joined)
}

fn create_dir_all_no_symlink(root: &Path, dir: &Path) -> Result<(), OciError> {
    if !dir.starts_with(root) {
        return Err(OciError::UnsafePath(format!(
            "path traversal rejected: {}",
            dir.display()
        )));
    }
    let relative = dir
        .strip_prefix(root)
        .map_err(|_| OciError::UnsafePath(format!("path traversal rejected: {}", dir.display())))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            continue;
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(OciError::UnsafePath(format!(
                    "symlink prefix rejected: {}",
                    current.display()
                )));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(OciError::UnsafePath(format!(
                    "non-directory prefix rejected: {}",
                    current.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(OciError::Io(error)),
        }
    }
    Ok(())
}

fn ensure_existing_dir_no_symlink(root: &Path, dir: &Path) -> Result<(), OciError> {
    if !dir.starts_with(root) {
        return Err(OciError::UnsafePath(format!(
            "path traversal rejected: {}",
            dir.display()
        )));
    }
    let relative = dir
        .strip_prefix(root)
        .map_err(|_| OciError::UnsafePath(format!("path traversal rejected: {}", dir.display())))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            continue;
        }
        current.push(component);
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() {
            return Err(OciError::UnsafePath(format!(
                "symlink prefix rejected: {}",
                current.display()
            )));
        }
        if !meta.is_dir() {
            return Err(OciError::UnsafePath(format!(
                "non-directory prefix rejected: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn validate_symlink_target(target: &Path, rootfs_dir: &Path) -> Result<(), OciError> {
    // Symlink targets are stored verbatim. Absolute targets and `..`
    // components are normal in OCI base images (e.g. `/usr/bin/awk ->
    // /usr/bin/mawk`, `/sbin/init -> ../bin/systemd`) and are inert until
    // followed. Workload isolation comes from pivot_root + mount namespace
    // at runtime, where absolute targets resolve inside the new root and
    // escapes are not possible. Only reject targets that resolve outside
    // the rootfs *and* point at an already-extracted path on the host,
    // which is the only case canonicalize can prove.
    if target.is_absolute() {
        return Ok(());
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

    fn gz_symlink_layer(dir: &Path, name: &str, link: &str, target: &Path) -> LayerBlob {
        let mut tar = Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_cksum();
        tar.append_link(&mut h, link, target).unwrap();
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
    fn rejects_file_write_through_symlink_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let host_target = dir.path().join("host-target");
        fs::create_dir_all(&host_target).unwrap();

        let l1 = gz_symlink_layer(dir.path(), "l1.tar.gz", "escape", &host_target);
        let l2 = gz_layer(dir.path(), "l2.tar.gz", &[("escape/pwned.txt", b"owned")]);
        let err = TarRootfsUnpacker::new()
            .unpack(&[l1, l2], &rootfs)
            .expect_err("symlink prefixes must not be followed during extraction");

        assert!(matches!(err, OciError::UnsafePath(_)), "{err:?}");
        assert!(!host_target.join("pwned.txt").exists());
    }

    #[test]
    fn rejects_whiteout_through_symlink_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let host_target = dir.path().join("host-target");
        fs::create_dir_all(&host_target).unwrap();
        fs::write(host_target.join("victim"), b"keep").unwrap();

        let l1 = gz_symlink_layer(dir.path(), "l1.tar.gz", "escape", &host_target);
        let l2 = gz_layer(dir.path(), "l2.tar.gz", &[("escape/.wh.victim", b"")]);
        let err = TarRootfsUnpacker::new()
            .unpack(&[l1, l2], &rootfs)
            .expect_err("whiteout parents must not follow symlink prefixes");

        assert!(matches!(err, OciError::UnsafePath(_)), "{err:?}");
        assert_eq!(fs::read(host_target.join("victim")).unwrap(), b"keep");
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
    fn symlink_replaces_existing_directory_across_layers() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        // Layer 1 materializes a real directory `lib/` (via its child file).
        let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("lib/old.so", b"x")]);
        // Layer 2 redefines `lib` as a symlink (usrmerge-style). Pre-fix this
        // failed EEXIST because `remove_file` cannot clear the directory.
        let l2 = gz_symlink_layer(dir.path(), "l2.tar.gz", "lib", Path::new("/usr/lib"));
        TarRootfsUnpacker::new().unpack(&[l1, l2], &rootfs).unwrap();
        let meta = fs::symlink_metadata(rootfs.join("lib")).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "lib must be replaced by a symlink across layers"
        );
        assert!(
            !rootfs.join("lib/old.so").exists(),
            "the replaced directory's contents must be gone"
        );
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

    fn gz_layer_with_modes(dir: &Path, name: &str, entries: &[(&str, &[u8], u32)]) -> LayerBlob {
        let mut tar = Builder::new(Vec::new());
        for (path, data, mode) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(*mode);
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
    fn strips_suid_sgid_sticky_bits() {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        let layer = gz_layer_with_modes(
            dir.path(),
            "suid.tar.gz",
            &[
                ("suid_bin", b"#!/bin/sh", 0o4755),
                ("sgid_bin", b"#!/bin/sh", 0o2755),
                ("sticky_file", b"data", 0o1777),
                ("normal", b"ok", 0o644),
            ],
        );
        TarRootfsUnpacker::new().unpack(&[layer], &rootfs).unwrap();
        let suid_mode = fs::metadata(rootfs.join("suid_bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(suid_mode & 0o7777, 0o755, "SUID bit should be stripped");
        let sgid_mode = fs::metadata(rootfs.join("sgid_bin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(sgid_mode & 0o7777, 0o755, "SGID bit should be stripped");
        let sticky_mode = fs::metadata(rootfs.join("sticky_file"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(sticky_mode & 0o7777, 0o777, "sticky bit should be stripped");
        let normal_mode = fs::metadata(rootfs.join("normal"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            normal_mode & 0o7777,
            0o644,
            "normal mode should be preserved"
        );
    }
}
