//! Privileged console launcher: fork a child that joins a tracked replica's
//! namespaces + cgroup, attaches a PTY slave as its controlling terminal,
//! chroots into the replica rootfs, and execs `/bin/sh`. See ADR-033.
//!
//! This is a NEW syscall path; it deliberately does not touch
//! `spawn_namespaced_process`, which owns the service/job launch flow.

use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::syscall::SyscallError;

#[derive(Debug, Clone)]
pub struct ConsoleLaunchConfig {
    pub target_pid: u32,
    pub cgroup_path: PathBuf,
    pub rootfs: PathBuf,
    pub workdir: String,
    pub env: Vec<(String, String)>,
    pub shell: String,
}

/// Fork a console child against `config`, handing it the PTY `slave` fd. Returns
/// the child pid in the parent. The child never returns — it execs the shell or
/// `_exit`s on any setup failure.
pub fn spawn_console_process(
    config: &ConsoleLaunchConfig,
    slave: OwnedFd,
) -> Result<u32, SyscallError> {
    validate_console_config(config)?;
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    if pid == 0 {
        unsafe {
            child_exec_console(config, slave.as_raw_fd());
        }
    }
    drop(slave);
    Ok(pid as u32)
}

fn validate_console_config(config: &ConsoleLaunchConfig) -> Result<(), SyscallError> {
    if config.target_pid == 0 {
        return Err(SyscallError::Capability(
            "target pid must be non-zero".to_string(),
        ));
    }
    if !config.rootfs.is_absolute() {
        return Err(SyscallError::Capability(
            "rootfs must be absolute".to_string(),
        ));
    }
    if !config.cgroup_path.is_absolute() {
        return Err(SyscallError::Capability(
            "cgroup path must be absolute".to_string(),
        ));
    }
    if config.shell != "/bin/sh" {
        return Err(SyscallError::Capability(
            "console shell must be /bin/sh".to_string(),
        ));
    }
    Ok(())
}

unsafe fn child_exec_console(config: &ConsoleLaunchConfig, slave_fd: RawFd) -> ! {
    if child_exec_console_inner(config, slave_fd).is_err() {
        unsafe { libc::_exit(127) };
    }
    unsafe { libc::_exit(127) };
}

fn child_exec_console_inner(
    config: &ConsoleLaunchConfig,
    slave_fd: RawFd,
) -> Result<(), SyscallError> {
    join_namespace(config.target_pid, "user")?;
    join_namespace(config.target_pid, "mnt")?;
    join_namespace(config.target_pid, "net")?;
    join_namespace(config.target_pid, "uts")?;
    join_namespace(config.target_pid, "ipc")?;

    attach_self_to_cgroup(&config.cgroup_path.join("cgroup.procs"))?;
    make_controlling_terminal(slave_fd)?;
    chroot_into(&config.rootfs, &config.workdir)?;

    let shell = CString::new(config.shell.as_bytes())
        .map_err(|_| SyscallError::Capability("shell contains nul".to_string()))?;
    let arg0 = shell.clone();
    let argv = [arg0.as_ptr(), std::ptr::null()];
    let env = config
        .env
        .iter()
        .map(|(key, value)| {
            CString::new(format!("{key}={value}")).map_err(|_| {
                SyscallError::Capability("environment entry contains nul".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut env_ptrs = env.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    env_ptrs.push(std::ptr::null());
    unsafe {
        libc::execve(shell.as_ptr(), argv.as_ptr(), env_ptrs.as_ptr());
    }
    Err(SyscallError::Io(std::io::Error::last_os_error()))
}

fn join_namespace(pid: u32, name: &str) -> Result<(), SyscallError> {
    let path = format!("/proc/{pid}/ns/{name}");
    let file = std::fs::File::open(&path).map_err(SyscallError::Io)?;
    let rc = unsafe { libc::setns(file.as_raw_fd(), 0) };
    if rc == -1 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn attach_self_to_cgroup(path: &Path) -> Result<(), SyscallError> {
    std::fs::write(path, format!("{}\n", std::process::id())).map_err(SyscallError::Io)
}

fn make_controlling_terminal(slave_fd: RawFd) -> Result<(), SyscallError> {
    unsafe {
        libc::setsid();
        if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if libc::dup2(slave_fd, fd) == -1 {
                return Err(SyscallError::Io(std::io::Error::last_os_error()));
            }
        }
    }
    Ok(())
}

fn chroot_into(rootfs: &Path, workdir: &str) -> Result<(), SyscallError> {
    let root = CString::new(rootfs.as_os_str().as_encoded_bytes())
        .map_err(|_| SyscallError::Capability("rootfs contains nul".to_string()))?;
    let workdir = CString::new(workdir.as_bytes())
        .map_err(|_| SyscallError::Capability("workdir contains nul".to_string()))?;
    unsafe {
        if libc::chroot(root.as_ptr()) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
        if libc::chdir(workdir.as_ptr()) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ConsoleLaunchConfig {
        ConsoleLaunchConfig {
            target_pid: 42,
            cgroup_path: PathBuf::from("/sys/fs/cgroup/denia/x"),
            rootfs: PathBuf::from("/var/lib/denia/x/merged"),
            workdir: "/".to_string(),
            env: Vec::new(),
            shell: "/bin/sh".to_string(),
        }
    }

    #[test]
    fn validate_rejects_zero_pid() {
        let mut config = base_config();
        config.target_pid = 0;
        assert!(validate_console_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_relative_rootfs() {
        let mut config = base_config();
        config.rootfs = PathBuf::from("relative/merged");
        assert!(validate_console_config(&config).is_err());
    }

    #[test]
    fn validate_rejects_non_sh_shell() {
        let mut config = base_config();
        config.shell = "/bin/bash".to_string();
        assert!(validate_console_config(&config).is_err());
    }

    #[test]
    fn validate_accepts_well_formed_config() {
        assert!(validate_console_config(&base_config()).is_ok());
    }
}
