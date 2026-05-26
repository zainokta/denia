use rustix::process::{Pid, Signal, WaitOptions};

use crate::syscall::SyscallError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Exited(i32),
    Signaled(i32),
}

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

pub fn try_wait(pid: u32) -> Result<ProcessStatus, SyscallError> {
    wait_with_options(pid, WaitOptions::NOHANG)
}

pub fn wait(pid: u32) -> Result<ProcessStatus, SyscallError> {
    wait_with_options(pid, WaitOptions::empty())
}

fn wait_with_options(pid: u32, options: WaitOptions) -> Result<ProcessStatus, SyscallError> {
    let raw_pid = Pid::from_raw(pid as i32).ok_or(SyscallError::Signal {
        pid,
        reason: "invalid pid".to_string(),
    })?;
    match rustix::process::waitpid(Some(raw_pid), options).map_err(|e| SyscallError::Signal {
        pid,
        reason: e.to_string(),
    })? {
        None => Ok(ProcessStatus::Running),
        Some((_pid, status)) => {
            if let Some(code) = status.exit_status() {
                Ok(ProcessStatus::Exited(code))
            } else if let Some(signal) = status.terminating_signal() {
                Ok(ProcessStatus::Signaled(signal))
            } else {
                Ok(ProcessStatus::Running)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_reports_child_exit_status() {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .expect("child");
        let pid = child.id();

        let status = wait(pid).expect("wait status");

        assert_eq!(status, ProcessStatus::Exited(7));
        std::mem::forget(child);
    }

    #[test]
    fn try_wait_reports_running_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("child");
        let pid = child.id();

        let status = try_wait(pid).expect("try wait status");

        assert_eq!(status, ProcessStatus::Running);
        child.kill().expect("kill child");
        let _ = wait(pid);
        std::mem::forget(child);
    }
}
