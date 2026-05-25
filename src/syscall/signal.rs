use rustix::process::{Pid, Signal};

use crate::syscall::SyscallError;

pub fn kill(pid: u32, sig: Signal) -> Result<(), SyscallError> {
    let raw_pid = Pid::from_raw(pid as i32).ok_or(SyscallError::Signal {
        pid,
        reason: "invalid pid".to_string(),
    })?;
    rustix::process::kill_process(raw_pid, sig).map_err(|e| SyscallError::Signal {
        pid,
        reason: e.to_string(),
    })
}
