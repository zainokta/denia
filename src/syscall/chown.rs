use std::path::Path;

use rustix::fs::{AtFlags, chownat};
use rustix::process::{Gid, Uid};

use crate::syscall::SyscallError;

pub fn recursive_lchown(root: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    chown_entry(root, uid, gid)?;
    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            recursive_lchown(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
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
}
