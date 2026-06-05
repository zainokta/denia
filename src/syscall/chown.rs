use std::{os::unix::fs::MetadataExt, path::Path};

use rustix::fs::{AtFlags, chownat};
use rustix::process::{Gid, Uid};

use crate::syscall::SyscallError;

pub fn recursive_lchown(root: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    // Post-order: enumerate and recurse BEFORE chowning `root` itself.
    // Chowning a directory away from the daemon's uid drops it to the dir's
    // "other" mode bits; a subsequent `read_dir` on a mode-0700 dir (common in
    // OCI base images, e.g. `/root`) would then fail EACCES because the daemon
    // holds CAP_CHOWN but not CAP_DAC_READ_SEARCH. Reading while still owned by
    // the daemon avoids that; the entry is chowned only after it is fully
    // traversed.
    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            recursive_lchown(&entry.path(), uid, gid)?;
        }
    }
    chown_entry(root, uid, gid)?;
    Ok(())
}

pub fn recursive_lchown_shifted(root: &Path, base: u32, size: u32) -> Result<(), SyscallError> {
    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            recursive_lchown_shifted(&entry.path(), base, size)?;
        }
    }
    let uid = shift_id(metadata.uid(), base, size);
    let gid = shift_id(metadata.gid(), base, size);
    chown_entry(root, uid, gid)?;
    Ok(())
}

fn shift_id(id: u32, base: u32, size: u32) -> u32 {
    if id >= base && id < base.saturating_add(size) {
        id
    } else if id < size {
        base.saturating_add(id)
    } else {
        base
    }
}

/// Change ownership of a single path without recursing or following symlinks.
///
/// Used to reclaim ownership of a directory the daemon created earlier but that
/// a workload run left owned by the userns base uid. The daemon holds CAP_CHOWN
/// but not CAP_FOWNER, so it must own a path before it can `chmod` it.
pub fn lchown(path: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    chown_entry(path, uid, gid)
}

fn chown_entry(path: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    let uid = Uid::from_raw(uid);
    let gid = Gid::from_raw(gid);
    chownat(
        rustix::fs::CWD,
        path,
        Some(uid),
        Some(gid),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| SyscallError::Io(e.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn recursive_lchown_does_not_recurse_through_directory_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        let outside_file = outside.join("owned-by-test-user");
        std::fs::write(&outside_file, "outside").expect("outside file");
        symlink(&outside, root.join("link-to-outside")).expect("symlink");

        let uid = std::fs::symlink_metadata(&outside_file)
            .expect("outside metadata")
            .uid();
        let gid = std::fs::symlink_metadata(&outside_file)
            .expect("outside metadata")
            .gid();

        recursive_lchown(&root, uid, gid).expect("recursive chown");

        assert_eq!(
            std::fs::symlink_metadata(&outside_file)
                .expect("outside metadata after chown")
                .uid(),
            uid,
            "must not recurse into symlink targets"
        );
    }

    #[test]
    fn shift_id_maps_image_ids_into_userns_range() {
        assert_eq!(shift_id(0, 100000, 65536), 100000);
        assert_eq!(shift_id(101, 100000, 65536), 100101);
        assert_eq!(shift_id(100101, 100000, 65536), 100101);
        assert_eq!(shift_id(70000, 100000, 65536), 100000);
    }
}
