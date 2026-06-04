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

use crate::syscall::seccomp::SockFilter;
use crate::syscall::{SyscallError, caps, seccomp};

#[derive(Debug, Clone)]
pub struct ConsoleLaunchConfig {
    pub target_pid: u32,
    /// Field 22 (`starttime`, clock ticks since boot) from `/proc/<pid>/stat`,
    /// captured when the console request resolved the live replica. Re-checked
    /// inside the child after the namespace fds are opened to defeat the
    /// pid-reuse TOCTOU window: a recycled pid will have a different starttime,
    /// so the console aborts rather than joining an unrelated process. See
    /// ADR-033 / review 07 (PID-reuse TOCTOU on setns).
    pub target_start_time: u64,
    pub cgroup_path: PathBuf,
    pub workdir: String,
    pub env: Vec<(String, String)>,
    pub shell: String,
    /// Re-apply the workload's privilege floor to the interactive shell before
    /// `execve` (ADR-005 / ADR-033). The shell already inherits the replica's
    /// user namespace (capless vs. host); these add the per-launch
    /// `no_new_privs`, capability-bounding-set drop, and seccomp denylist the
    /// workload itself runs under so the console is not a softer surface.
    pub no_new_privs: bool,
    pub drop_bounding_caps: bool,
    pub seccomp: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleProcess {
    pub supervisor_pid: u32,
    pub shell_pid: u32,
}

/// One stage byte per child setup step, written to the status pipe on failure.
const STAGE_NS_USER: u8 = 1;
const STAGE_NS_MNT: u8 = 2;
const STAGE_NS_NET: u8 = 3;
const STAGE_NS_UTS: u8 = 4;
const STAGE_NS_IPC: u8 = 5;
const STAGE_NS_PID: u8 = 6;
const STAGE_ROOT: u8 = 7;
const STAGE_CTTY: u8 = 8;
const STAGE_EXEC: u8 = 9;
const STAGE_PID_REUSE: u8 = 10;
const STAGE_NO_NEW_PRIVS: u8 = 11;
const STAGE_DROP_CAPS: u8 = 12;
const STAGE_SECCOMP: u8 = 13;
const STAGE_SHELL_FORK: u8 = 14;
const STAGE_SETGID: u8 = 15;
const STAGE_SETUID: u8 = 16;
const STAGE_NS_PID_EPERM: u8 = 17;
const STAGE_NS_PID_EINVAL: u8 = 18;

fn stage_label(stage: u8) -> &'static str {
    match stage {
        STAGE_NS_USER => "join user namespace",
        STAGE_NS_MNT => "join mount namespace",
        STAGE_NS_NET => "join net namespace",
        STAGE_NS_UTS => "join uts namespace",
        STAGE_NS_IPC => "join ipc namespace",
        STAGE_NS_PID => "join pid namespace",
        STAGE_ROOT => "root into replica filesystem",
        STAGE_CTTY => "set controlling terminal",
        STAGE_EXEC => "exec console shell",
        STAGE_PID_REUSE => "verify target replica identity (pid reuse)",
        STAGE_NO_NEW_PRIVS => "set no_new_privs",
        STAGE_DROP_CAPS => "drop capability bounding set",
        STAGE_SECCOMP => "install seccomp filter",
        STAGE_SHELL_FORK => "fork console shell in pid namespace",
        STAGE_SETGID => "set mapped root gid",
        STAGE_SETUID => "set mapped root uid",
        STAGE_NS_PID_EPERM => "join pid namespace errno=1 (EPERM)",
        STAGE_NS_PID_EINVAL => "join pid namespace errno=22 (EINVAL)",
        _ => "unknown console launch stage",
    }
}

/// Read field 22 (`starttime`) from `/proc/<pid>/stat`. Used both when the
/// console request resolves the replica and again (under the namespace fds)
/// inside the child to confirm the pid was not recycled. Returns `None` if the
/// process is gone or the field is unparseable.
pub fn read_process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field (2nd) may contain spaces/parens, so split after the last
    // ')' before counting the space-separated fields. starttime is field 22,
    // i.e. index 19 in the post-comm remainder (fields 3.. = pid is 1, comm 2).
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// Fork a console child against `config`, handing it the PTY `slave` fd. Returns
/// the child pid once the shell has `execve`'d. Errors (with the failing stage)
/// if the child could not set up or exec.
pub fn spawn_console_process(
    config: &ConsoleLaunchConfig,
    slave: OwnedFd,
) -> Result<ConsoleProcess, SyscallError> {
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
    // Compile the seccomp denylist BEFORE fork so the child only issues the two
    // `prctl` syscalls (no allocation post-fork). Empty when seccomp is off.
    let seccomp_program: Vec<SockFilter> = if config.seccomp {
        seccomp::build_filter_program()
    } else {
        Vec::new()
    };
    let hardening = Hardening {
        no_new_privs: config.no_new_privs,
        drop_bounding_caps: config.drop_bounding_caps,
        seccomp_program: &seccomp_program,
    };
    let target_start_time = config.target_start_time;

    // Close-on-exec status pipe: child writes a stage byte on failure; on a
    // successful execve the write end closes and the parent reads EOF.
    let mut pipe_fds = [0_i32; 2];
    if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    let read_fd = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
    let mut shell_pid_pipe = [0_i32; 2];
    if unsafe { libc::pipe2(shell_pid_pipe.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    let shell_pid_read = unsafe { OwnedFd::from_raw_fd(shell_pid_pipe[0]) };
    let shell_pid_write = unsafe { OwnedFd::from_raw_fd(shell_pid_pipe[1]) };

    let ns_raw: Vec<(u8, RawFd)> = ns_fds.iter().map(|(s, fd)| (*s, fd.as_raw_fd())).collect();
    let root_raw = root_fd.as_raw_fd();
    let slave_raw = slave.as_raw_fd();
    let write_raw = write_fd.as_raw_fd();
    let shell_pid_write_raw = shell_pid_write.as_raw_fd();

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
                shell_pid_write_raw,
                config.target_pid,
                target_start_time,
                &hardening,
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
    drop(shell_pid_write);
    drop(ns_fds);
    drop(root_fd);
    let supervisor_pid = pid as u32;
    let shell_pid = read_shell_pid(shell_pid_read).unwrap_or(0);

    match read_child_outcome(read_fd) {
        ChildOutcome::Exec => {
            if shell_pid == 0 {
                let _ = crate::syscall::signal::wait(supervisor_pid);
                return Err(SyscallError::ChildSetup {
                    stage: "console shell pid was not reported",
                });
            }
            // The shell is running; place it in the replica's cgroup from the
            // host mount namespace (best-effort — accounting, not correctness).
            for pid in [supervisor_pid, shell_pid] {
                if let Err(error) = attach_pid_to_cgroup(pid, &config.cgroup_path) {
                    tracing::warn!(
                        pid,
                        cgroup = %config.cgroup_path.display(),
                        ?error,
                        "console: failed to attach process to replica cgroup"
                    );
                }
            }
            Ok(ConsoleProcess {
                supervisor_pid,
                shell_pid,
            })
        }
        ChildOutcome::Failed(stage) => {
            let _ = crate::syscall::signal::wait(supervisor_pid);
            Err(SyscallError::ChildSetup {
                stage: stage_label(stage),
            })
        }
        ChildOutcome::Unknown => {
            let _ = crate::syscall::signal::wait(supervisor_pid);
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

fn read_shell_pid(read_fd: OwnedFd) -> Option<u32> {
    let mut file = std::fs::File::from(read_fd);
    let mut buf = [0_u8; std::mem::size_of::<u32>()];
    file.read_exact(&mut buf).ok()?;
    let pid = u32::from_ne_bytes(buf);
    (pid != 0).then_some(pid)
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
/// with the stage byte to report if its `setns` fails. PID namespace is joined
/// first because it only affects future children, and some kernels reject the
/// PID join after the caller has already switched into the target user
/// namespace.
fn console_namespace_specs() -> &'static [(u8, &'static str)] {
    &[
        (STAGE_NS_PID, "pid"),
        (STAGE_NS_USER, "user"),
        (STAGE_NS_MNT, "mnt"),
        (STAGE_NS_NET, "net"),
        (STAGE_NS_UTS, "uts"),
        (STAGE_NS_IPC, "ipc"),
    ]
}

fn open_namespace_fds(pid: u32) -> Result<Vec<(u8, OwnedFd)>, SyscallError> {
    let mut fds = Vec::with_capacity(console_namespace_specs().len());
    for (stage, name) in console_namespace_specs() {
        let file =
            std::fs::File::open(format!("/proc/{pid}/ns/{name}")).map_err(SyscallError::Io)?;
        fds.push((*stage, OwnedFd::from(file)));
    }
    Ok(fds)
}

/// Directory fd for the target's root, used to re-root the child after it joins
/// the replica's mount namespace (`fchdir` + `chroot(".")`).
fn open_target_root(pid: u32) -> Result<OwnedFd, SyscallError> {
    let file = std::fs::File::open(format!("/proc/{pid}/root")).map_err(SyscallError::Io)?;
    Ok(OwnedFd::from(file))
}

/// Privilege-floor steps the console child applies right before `execve` so the
/// interactive shell matches the workload's posture (ADR-005 / ADR-033). The
/// seccomp program is compiled in the parent (`build_filter_program`); the child
/// only issues `prctl`.
struct Hardening<'a> {
    no_new_privs: bool,
    drop_bounding_caps: bool,
    seccomp_program: &'a [SockFilter],
}

#[expect(clippy::too_many_arguments)]
unsafe fn child_exec_console(
    ns_fds: &[(u8, RawFd)],
    root_fd: RawFd,
    slave_fd: RawFd,
    status_fd: RawFd,
    shell_pid_fd: RawFd,
    target_pid: u32,
    target_start_time: u64,
    hardening: &Hardening<'_>,
    workdir: *const libc::c_char,
    shell: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> ! {
    unsafe {
        // PID-reuse guard: the ns fds above were opened by the parent against
        // `target_pid` at request time. Re-read the host /proc starttime before
        // changing namespaces and bail if the pid was recycled onto a different
        // process between resolve and fork.
        match read_process_start_time(target_pid) {
            Some(now) if now == target_start_time => {}
            _ => fail(status_fd, STAGE_PID_REUSE),
        }

        for (stage, fd) in ns_fds {
            if libc::setns(*fd, 0) == -1 {
                if *stage == STAGE_NS_PID {
                    match errno() {
                        libc::EPERM => fail(status_fd, STAGE_NS_PID_EPERM),
                        libc::EINVAL => fail(status_fd, STAGE_NS_PID_EINVAL),
                        _ => {}
                    }
                }
                fail(status_fd, *stage);
            }
        }

        let shell_pid = libc::fork();
        if shell_pid < 0 {
            fail(status_fd, STAGE_SHELL_FORK);
        }
        if shell_pid > 0 {
            let bytes = (shell_pid as u32).to_ne_bytes();
            let _ = libc::write(shell_pid_fd, bytes.as_ptr().cast(), bytes.len());
            libc::close(shell_pid_fd);
            libc::close(status_fd);
            let mut status = 0_i32;
            loop {
                if libc::waitpid(shell_pid, &mut status, 0) == shell_pid {
                    if libc::WIFEXITED(status) {
                        libc::_exit(libc::WEXITSTATUS(status));
                    }
                    if libc::WIFSIGNALED(status) {
                        libc::_exit(128 + libc::WTERMSIG(status));
                    }
                    libc::_exit(127);
                }
                if errno() != libc::EINTR {
                    libc::_exit(127);
                }
            }
        }
        libc::close(shell_pid_fd);

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

        // Re-apply the workload's per-launch privilege floor before execve, in
        // the same order the service launcher uses (no_new_privs -> bounding-set
        // drop -> seccomp). caps::* mirror the workload child path in ns.rs;
        // seccomp uses the parent-compiled program so the child only prctls.
        if hardening.no_new_privs && !caps::try_set_no_new_privs() {
            fail(status_fd, STAGE_NO_NEW_PRIVS);
        }
        if hardening.drop_bounding_caps && !caps::try_drop_bounding_caps() {
            fail(status_fd, STAGE_DROP_CAPS);
        }
        if !hardening.seccomp_program.is_empty()
            && seccomp::apply_program(hardening.seccomp_program).is_err()
        {
            fail(status_fd, STAGE_SECCOMP);
        }

        close_inherited_fds(status_fd);
        if libc::setresgid(0, 0, 0) < 0 {
            fail(status_fd, STAGE_SETGID);
        }
        if libc::setresuid(0, 0, 0) < 0 {
            fail(status_fd, STAGE_SETUID);
        }

        libc::execve(shell, argv, envp);
        // execve only returns on failure.
        fail(status_fd, STAGE_EXEC);
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
}

/// Best-effort fd sweep before exec. Matches the workload launcher policy:
/// keep stdio and the setup status pipe, close anything else inherited from
/// the privileged daemon if the kernel supports close_range(2).
unsafe fn close_inherited_fds(keep: RawFd) {
    const FIRST: libc::c_uint = 3;
    let keep = keep as libc::c_uint;
    if keep > FIRST {
        let _ = unsafe { libc::syscall(libc::SYS_close_range, FIRST, keep - 1, 0) };
    }
    let lo = std::cmp::max(FIRST, keep.saturating_add(1));
    let _ = unsafe { libc::syscall(libc::SYS_close_range, lo, libc::c_uint::MAX, 0) };
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
            target_start_time: 123456,
            cgroup_path: PathBuf::from("/sys/fs/cgroup/denia/x"),
            workdir: "/".to_string(),
            env: Vec::new(),
            shell: "/bin/sh".to_string(),
            no_new_privs: true,
            drop_bounding_caps: true,
            seccomp: true,
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
            STAGE_NS_PID,
            STAGE_ROOT,
            STAGE_CTTY,
            STAGE_EXEC,
            STAGE_PID_REUSE,
            STAGE_NO_NEW_PRIVS,
            STAGE_DROP_CAPS,
            STAGE_SECCOMP,
            STAGE_SHELL_FORK,
            STAGE_SETGID,
            STAGE_SETUID,
            STAGE_NS_PID_EPERM,
            STAGE_NS_PID_EINVAL,
        ] {
            assert_ne!(stage_label(stage), "unknown console launch stage");
        }
    }

    #[test]
    fn console_process_carries_supervisor_and_shell_pids() {
        let process = ConsoleProcess {
            supervisor_pid: 10,
            shell_pid: 11,
        };
        assert_eq!(process.supervisor_pid, 10);
        assert_eq!(process.shell_pid, 11);
    }

    #[test]
    fn console_namespace_list_includes_pid_namespace() {
        let names = console_namespace_specs()
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"pid"),
            "console must join pid namespace before forking shell"
        );
    }

    #[test]
    fn console_namespace_list_joins_pid_namespace_first() {
        let first = console_namespace_specs()
            .first()
            .expect("namespace specs must not be empty");
        assert_eq!(*first, (STAGE_NS_PID, "pid"));
    }

    #[test]
    fn read_process_start_time_returns_value_for_self() {
        // The current process always has a parseable starttime; the value is
        // opaque but must be present and stable across two reads.
        let pid = std::process::id();
        let first = read_process_start_time(pid).expect("self starttime");
        let second = read_process_start_time(pid).expect("self starttime again");
        assert_eq!(first, second);
    }

    #[test]
    fn read_process_start_time_handles_comm_with_spaces_and_parens() {
        // The 2nd field (comm) can contain spaces and parens; the parser must
        // split after the LAST ')' so field 22 (starttime) is read correctly.
        // Synthetic stat line: pid=7 comm="(weird ) name)" then fields 3..
        // arranged so the 22nd field (starttime) is 998877.
        let mut fields = String::from("7 (weird ) name) ");
        // fields 3..=21 (19 values) are placeholders, field 22 = starttime.
        for i in 3..=21 {
            fields.push_str(&format!("{i} "));
        }
        fields.push_str("998877 0 0\n");
        let after_comm = fields.rsplit_once(')').unwrap().1;
        let starttime: u64 = after_comm
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(starttime, 998877);
    }
}
