//! Privileged console launcher: fork a child that joins a tracked replica's
//! namespaces + cgroup, attaches a PTY slave as its controlling terminal, roots
//! into the replica's filesystem, and execs `/bin/sh`. See ADR-033.
//!
//! This is a NEW syscall path; it deliberately does not touch
//! `spawn_namespaced_process`, which owns the service/job launch flow.
//!
//! Design notes (why it looks the way it does):
//! - The replica already created + pivoted into its mount namespace, so we
//!   *join* it (`setns`) rather than building one. After joining the mount
//!   namespace the child's root/cwd are stale, so we re-root via a
//!   `/proc/<pid>/root` directory fd (`fchdir` + `chroot(".")`), exactly like
//!   `nsenter -r`. Chrooting to a host path (e.g. the overlay `merged`
//!   mountpoint) would fail: that path does not exist inside the replica's
//!   pivoted mount namespace.
//! - The cgroup join is done by the PARENT in the host mount namespace
//!   (`/sys/fs/cgroup/...` is not visible once we `setns(mnt)`), matching the
//!   service launcher.
//! - Everything the child needs (namespace fds, the root-dir fd, and all
//!   `CString`s) is allocated in the parent BEFORE `fork`, so the child path
//!   makes only async-signal-safe raw syscalls — no allocation after `fork` in
//!   the multi-threaded daemon.
//! - The child reports the stage it failed at over a close-on-exec status pipe;
//!   on a successful `execve` the pipe closes and the parent reads EOF. This
//!   turns an opaque "child exited, PTY read = EIO" into a precise error.

use std::ffi::CString;
use std::io::Read as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

use crate::syscall::SyscallError;

#[derive(Debug, Clone)]
pub struct ConsoleLaunchConfig {
    pub target_pid: u32,
    pub cgroup_path: PathBuf,
    pub workdir: String,
    pub env: Vec<(String, String)>,
    pub shell: String,
}

/// One stage byte per child setup step, written to the status pipe on failure.
const STAGE_NS_USER: u8 = 1;
const STAGE_NS_MNT: u8 = 2;
const STAGE_NS_NET: u8 = 3;
const STAGE_NS_UTS: u8 = 4;
const STAGE_NS_IPC: u8 = 5;
const STAGE_ROOT: u8 = 6;
const STAGE_CTTY: u8 = 7;
const STAGE_EXEC: u8 = 8;

fn stage_label(stage: u8) -> &'static str {
    match stage {
        STAGE_NS_USER => "join user namespace",
        STAGE_NS_MNT => "join mount namespace",
        STAGE_NS_NET => "join net namespace",
        STAGE_NS_UTS => "join uts namespace",
        STAGE_NS_IPC => "join ipc namespace",
        STAGE_ROOT => "root into replica filesystem",
        STAGE_CTTY => "set controlling terminal",
        STAGE_EXEC => "exec console shell",
        _ => "unknown console launch stage",
    }
}

/// Fork a console child against `config`, handing it the PTY `slave` fd. Returns
/// the child pid once the shell has `execve`'d. Errors (with the failing stage)
/// if the child could not set up or exec.
pub fn spawn_console_process(
    config: &ConsoleLaunchConfig,
    slave: OwnedFd,
) -> Result<u32, SyscallError> {
    validate_console_config(config)?;

    // --- Parent-side allocation (before fork; child must not allocate) ------
    let ns_fds = open_namespace_fds(config.target_pid)?;
    let root_fd = open_target_root(config.target_pid)?;
    let workdir = CString::new(config.workdir.as_bytes())
        .map_err(|_| SyscallError::Capability("workdir contains nul".to_string()))?;
    let shell = CString::new(config.shell.as_bytes())
        .map_err(|_| SyscallError::Capability("shell contains nul".to_string()))?;
    let argv: [*const libc::c_char; 2] = [shell.as_ptr(), std::ptr::null()];
    let env = config
        .env
        .iter()
        .map(|(key, value)| {
            CString::new(format!("{key}={value}"))
                .map_err(|_| SyscallError::Capability("environment entry contains nul".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut env_ptrs = env.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    env_ptrs.push(std::ptr::null());

    // Close-on-exec status pipe: child writes a stage byte on failure; on a
    // successful execve the write end closes and the parent reads EOF.
    let mut pipe_fds = [0_i32; 2];
    if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    let read_fd = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };

    let ns_raw: Vec<(u8, RawFd)> = ns_fds.iter().map(|(s, fd)| (*s, fd.as_raw_fd())).collect();
    let root_raw = root_fd.as_raw_fd();
    let slave_raw = slave.as_raw_fd();
    let write_raw = write_fd.as_raw_fd();

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    if pid == 0 {
        // Child: raw syscalls only.
        unsafe {
            child_exec_console(
                &ns_raw,
                root_raw,
                slave_raw,
                write_raw,
                workdir.as_ptr(),
                shell.as_ptr(),
                argv.as_ptr(),
                env_ptrs.as_ptr(),
            );
        }
    }

    // Parent: drop everything the child owns now, then wait for its outcome.
    drop(slave);
    drop(write_fd);
    drop(ns_fds);
    drop(root_fd);
    let child_pid = pid as u32;

    match read_child_outcome(read_fd) {
        ChildOutcome::Exec => {
            // The shell is running; place it in the replica's cgroup from the
            // host mount namespace (best-effort — accounting, not correctness).
            if let Err(error) = attach_pid_to_cgroup(child_pid, &config.cgroup_path) {
                tracing::warn!(
                    pid = child_pid,
                    cgroup = %config.cgroup_path.display(),
                    ?error,
                    "console: failed to attach shell to replica cgroup"
                );
            }
            Ok(child_pid)
        }
        ChildOutcome::Failed(stage) => {
            let _ = crate::syscall::signal::wait(child_pid);
            Err(SyscallError::ChildSetup {
                stage: stage_label(stage),
            })
        }
        ChildOutcome::Unknown => {
            let _ = crate::syscall::signal::wait(child_pid);
            Err(SyscallError::ChildSetup {
                stage: "console child failed before exec",
            })
        }
    }
}

enum ChildOutcome {
    Exec,
    Failed(u8),
    Unknown,
}

fn read_child_outcome(read_fd: OwnedFd) -> ChildOutcome {
    let mut file = std::fs::File::from(read_fd);
    let mut buf = [0_u8; 1];
    match file.read(&mut buf) {
        Ok(0) => ChildOutcome::Exec,
        Ok(_) => ChildOutcome::Failed(buf[0]),
        Err(_) => ChildOutcome::Unknown,
    }
}

fn validate_console_config(config: &ConsoleLaunchConfig) -> Result<(), SyscallError> {
    if config.target_pid == 0 {
        return Err(SyscallError::Capability(
            "target pid must be non-zero".to_string(),
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

/// Open `/proc/<pid>/ns/<name>` for each namespace the console joins, paired
/// with the stage byte to report if its `setns` fails. User namespace first so
/// the child gains the capabilities needed to join the rest.
fn open_namespace_fds(pid: u32) -> Result<Vec<(u8, OwnedFd)>, SyscallError> {
    let names = [
        (STAGE_NS_USER, "user"),
        (STAGE_NS_MNT, "mnt"),
        (STAGE_NS_NET, "net"),
        (STAGE_NS_UTS, "uts"),
        (STAGE_NS_IPC, "ipc"),
    ];
    let mut fds = Vec::with_capacity(names.len());
    for (stage, name) in names {
        let file =
            std::fs::File::open(format!("/proc/{pid}/ns/{name}")).map_err(SyscallError::Io)?;
        fds.push((stage, OwnedFd::from(file)));
    }
    Ok(fds)
}

/// Directory fd for the target's root, used to re-root the child after it joins
/// the replica's mount namespace (`fchdir` + `chroot(".")`).
fn open_target_root(pid: u32) -> Result<OwnedFd, SyscallError> {
    let file = std::fs::File::open(format!("/proc/{pid}/root")).map_err(SyscallError::Io)?;
    Ok(OwnedFd::from(file))
}

#[expect(clippy::too_many_arguments)]
unsafe fn child_exec_console(
    ns_fds: &[(u8, RawFd)],
    root_fd: RawFd,
    slave_fd: RawFd,
    status_fd: RawFd,
    workdir: *const libc::c_char,
    shell: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> ! {
    unsafe {
        for (stage, fd) in ns_fds {
            if libc::setns(*fd, 0) == -1 {
                fail(status_fd, *stage);
            }
        }

        // Re-root into the replica filesystem (nsenter -r equivalent): fchdir to
        // the pre-opened target root dir fd, then chroot to it.
        if libc::fchdir(root_fd) == -1 || libc::chroot(c".".as_ptr()) == -1 {
            fail(status_fd, STAGE_ROOT);
        }
        // Best-effort working directory; fall back to "/" so a missing workdir
        // never sinks the session.
        if libc::chdir(workdir) == -1 {
            let _ = libc::chdir(c"/".as_ptr());
        }

        // New session + controlling terminal on the PTY slave.
        libc::setsid();
        if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) == -1 {
            fail(status_fd, STAGE_CTTY);
        }
        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if libc::dup2(slave_fd, fd) == -1 {
                fail(status_fd, STAGE_CTTY);
            }
        }

        libc::execve(shell, argv, envp);
        // execve only returns on failure.
        fail(status_fd, STAGE_EXEC);
    }
}

/// Report the failing stage to the parent and terminate the child. Uses only
/// async-signal-safe calls.
unsafe fn fail(status_fd: RawFd, stage: u8) -> ! {
    let byte = [stage];
    unsafe {
        let _ = libc::write(status_fd, byte.as_ptr().cast(), 1);
        libc::_exit(127);
    }
}

/// Parent-side: write the child pid into the replica's `cgroup.procs` from the
/// host mount namespace. A single `write(2)` as the cgroup parser requires.
fn attach_pid_to_cgroup(pid: u32, cgroup_path: &std::path::Path) -> Result<(), SyscallError> {
    use std::io::Write as _;
    let procs = cgroup_path.join("cgroup.procs");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&procs)
        .map_err(SyscallError::Io)?;
    file.write_all(format!("{pid}\n").as_bytes())
        .map_err(SyscallError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ConsoleLaunchConfig {
        ConsoleLaunchConfig {
            target_pid: 42,
            cgroup_path: PathBuf::from("/sys/fs/cgroup/denia/x"),
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
    fn validate_rejects_relative_cgroup() {
        let mut config = base_config();
        config.cgroup_path = PathBuf::from("relative/cg");
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

    #[test]
    fn stage_labels_are_distinct_and_named() {
        for stage in [
            STAGE_NS_USER,
            STAGE_NS_MNT,
            STAGE_NS_NET,
            STAGE_NS_UTS,
            STAGE_NS_IPC,
            STAGE_ROOT,
            STAGE_CTTY,
            STAGE_EXEC,
        ] {
            assert_ne!(stage_label(stage), "unknown console launch stage");
        }
    }
}
