use rustix::thread;

use crate::syscall::SyscallError;

pub fn set_no_new_privs() -> Result<(), SyscallError> {
    thread::set_no_new_privs(true)
        .map_err(|e| SyscallError::Capability(format!("PR_SET_NO_NEW_PRIVS: {e}")))
}
