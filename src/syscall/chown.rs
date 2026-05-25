use std::path::Path;

use rustix::fs::{AtFlags, chownat};
use rustix::process::{Gid, Uid};

use crate::syscall::SyscallError;

pub fn recursive_lchown(root: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    chown_entry(root, uid, gid)?;
    if root.is_dir() {
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
