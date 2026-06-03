use std::path::{Component, Path};

pub struct ExtractLimits {
    pub max_uncompressed: u64,
    pub max_entries: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive rejected: {0}")]
    Rejected(String),
}

/// Extract a `tar.zst` into `dest`, accepting only regular files and dirs.
///
/// On error, partially-extracted files may remain in `dest`; cleanup is the caller's responsibility.
pub fn extract_tar_zst(bytes: &[u8], dest: &Path, limits: &ExtractLimits) -> Result<(), ExtractError> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut count: u64 = 0;
    let mut total: u64 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        count += 1;
        if count > limits.max_entries {
            return Err(ExtractError::Rejected("too many entries".into()));
        }
        let etype = entry.header().entry_type();
        if !(etype.is_file() || etype.is_dir()) {
            return Err(ExtractError::Rejected(format!("disallowed entry type: {etype:?}")));
        }
        let path = entry.path()?.into_owned();
        for c in path.components() {
            match c {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(ExtractError::Rejected(format!("unsafe path: {}", path.display()))),
            }
        }
        total = total.saturating_add(entry.header().size()?);
        if total > limits.max_uncompressed {
            return Err(ExtractError::Rejected("uncompressed size cap exceeded".into()));
        }
        if !entry.unpack_in(dest)? {
            return Err(ExtractError::Rejected(format!(
                "entry skipped/escaped: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, *body).unwrap();
        }
        let tar = tar.into_inner().unwrap();
        zstd::stream::encode_all(&tar[..], 0).unwrap()
    }

    #[test]
    fn extracts_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("Dockerfile", b"FROM scratch\n"), ("src/main.rs", b"fn main(){}")]);
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        extract_tar_zst(&bytes, dir.path(), &limits).unwrap();
        assert!(dir.path().join("Dockerfile").exists());
        assert!(dir.path().join("src/main.rs").exists());
    }

    /// Build a raw tar archive (uncompressed) with a single regular-file entry
    /// whose name is exactly `name` (no sanitisation). Used to inject paths like
    /// `../escape` that the tar::Builder itself would refuse to write.
    fn raw_tar_with_name(name: &str, body: &[u8]) -> Vec<u8> {
        // POSIX ustar header layout: name[100], mode[8], uid[8], gid[8],
        // size[12], mtime[12], checksum[8], typeflag[1], linkname[100],
        // magic[6], version[2], uname[32], gname[32], devmajor[8],
        // devminor[8], prefix[155], pad[12]  = 512 bytes total.
        let mut header = [0u8; 512];
        // name (bytes 0..100)
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(99);
        header[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        // mode (100..108): octal "0000644\0"
        header[100..107].copy_from_slice(b"0000644");
        header[107] = b'\0';
        // uid / gid: leave as zeros (valid enough for reading)
        // size (124..136): octal of body.len() + NUL  [POSIX ustar bytes 124–135]
        let size_str = format!("{:011o}\0", body.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        // mtime (136..148): "00000000000\0"  [POSIX ustar bytes 136–147]
        header[136..147].copy_from_slice(b"00000000000");
        header[147] = b'\0';
        // checksum (148..156): computed below  [POSIX ustar bytes 148–155]
        // typeflag (156): '0' = regular file
        header[156] = b'0';
        // magic / version (257..265): "ustar  \0"
        header[257..263].copy_from_slice(b"ustar ");
        header[263..265].copy_from_slice(b" \0");
        // Checksum (148..156): sum of all header bytes with chksum field treated as spaces,
        // stored as 6-digit octal + NUL + space.
        header[148..156].copy_from_slice(b"        "); // placeholder (8 spaces per POSIX)
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        let mut out = Vec::with_capacity(512 + body.len().div_ceil(512) * 512 + 1024);
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        // Pad data block to 512-byte boundary
        let pad = (512 - body.len() % 512) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
        // Two 512-byte zero blocks = end-of-archive
        out.extend([0u8; 1024]);
        out
    }

    fn raw_tar_zst_with_name(name: &str, body: &[u8]) -> Vec<u8> {
        let tar_bytes = raw_tar_with_name(name, body);
        zstd::stream::encode_all(&tar_bytes[..], 0).unwrap()
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = raw_tar_zst_with_name("../escape", b"x");
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_too_many_entries() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<(String, Vec<u8>)> =
            (0..10).map(|i| (format!("f{i}"), vec![0u8])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let bytes = tar_zst(&refs);
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 3,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_oversize_uncompressed() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("big", &vec![7u8; 4096])]);
        let limits = ExtractLimits {
            max_uncompressed: 1024,
            max_entries: 100,
        };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        // Build a tar with a symlink entry manually using the tar 0.4 API.
        let mut tar_buf = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        h.set_username("root").unwrap();
        h.set_link_name("/etc/passwd").unwrap();
        h.set_cksum();
        tar_buf
            .append_data(&mut h, "evil_link", std::io::empty())
            .unwrap();
        let tar_bytes = tar_buf.into_inner().unwrap();
        let compressed = zstd::stream::encode_all(&tar_bytes[..], 0).unwrap();
        let limits = ExtractLimits {
            max_uncompressed: 1 << 20,
            max_entries: 100,
        };
        assert!(
            extract_tar_zst(&compressed, dir.path(), &limits).is_err(),
            "symlink entry must be rejected"
        );
    }
}
