use std::{
    ffi::CString,
    io::Write,
    os::fd::RawFd,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use crate::syscall::SyscallError;

/// Configuration for a namespaced one-shot or long-running process launch.
///
/// `NamespaceConfig` is the in-process counterpart to the `unshare` CLI flags.
/// Building this struct describes *what* to unshare and *how* to map uids; the
/// actual fork + unshare + uid_map write happens in `spawn_namespaced_process`.
///
/// The struct is intentionally a builder around concrete fields so its public
/// surface is testable without needing root privileges. Privileged execution
/// is gated to `spawn_namespaced_process` which returns
/// `SyscallError::Capability` on non-root callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceConfig {
    pub rootfs: PathBuf,
    pub workdir: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cgroup_path: PathBuf,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
    pub userns: bool,
    pub uid_map: Option<UidMap>,
    pub gid_map: Option<UidMap>,
    pub pid_ns: bool,
    pub net_ns: bool,
    pub mount_ns: bool,
    pub uts_ns: bool,
    pub ipc_ns: bool,
    pub mount_proc: bool,
    pub no_new_privs: bool,
    pub drop_bounding_caps: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidMap {
    pub inside: u32,
    pub outside: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceSetupPlan {
    pub clone_flags: libc::c_int,
    pub uid_map: Option<String>,
    pub gid_map: Option<String>,
    pub cgroup_procs_path: PathBuf,
    pub deny_setgroups: bool,
    pub mount_proc: bool,
    pub no_new_privs: bool,
    pub drop_bounding_caps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLaunchPlan {
    pub setup: NamespaceSetupPlan,
    pub rootfs: CString,
    pub workdir: CString,
    pub program: CString,
    pub argv: Vec<CString>,
    pub env: Vec<CString>,
    pub proc_mount_target: CString,
    pub proc_fs_type: CString,
    pub stdout_path: Option<CString>,
    pub stderr_path: Option<CString>,
}

impl NamespaceSetupPlan {
    pub fn write_id_maps_for_pid(&self, pid: u32) -> Result<(), SyscallError> {
        self.write_id_maps_at(&PathBuf::from("/proc").join(pid.to_string()))
    }

    fn write_id_maps_at(&self, proc_pid_dir: &Path) -> Result<(), SyscallError> {
        if self.deny_setgroups {
            write_proc_setup_file(proc_pid_dir, "setgroups", "deny\n")?;
        }
        if let Some(uid_map) = &self.uid_map {
            write_proc_setup_file(proc_pid_dir, "uid_map", uid_map)?;
        }
        if let Some(gid_map) = &self.gid_map {
            write_proc_setup_file(proc_pid_dir, "gid_map", gid_map)?;
        }
        Ok(())
    }
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            rootfs: PathBuf::new(),
            workdir: "/".to_string(),
            argv: Vec::new(),
            env: Vec::new(),
            cgroup_path: PathBuf::new(),
            stdout_path: None,
            stderr_path: None,
            userns: true,
            uid_map: None,
            gid_map: None,
            pid_ns: true,
            net_ns: true,
            mount_ns: true,
            uts_ns: true,
            ipc_ns: true,
            mount_proc: true,
            no_new_privs: true,
            drop_bounding_caps: true,
        }
    }
}

impl NamespaceConfig {
    pub fn new(rootfs: impl Into<PathBuf>, argv: Vec<String>) -> Self {
        Self {
            rootfs: rootfs.into(),
            argv,
            ..Self::default()
        }
    }

    pub fn with_uid_map(mut self, base: u32, size: u32) -> Self {
        self.uid_map = Some(UidMap {
            inside: 0,
            outside: base,
            size,
        });
        self.gid_map = Some(UidMap {
            inside: 0,
            outside: base,
            size,
        });
        self
    }

    pub fn with_cgroup_path(mut self, cgroup_path: impl Into<PathBuf>) -> Self {
        self.cgroup_path = cgroup_path.into();
        self
    }

    pub fn with_workdir(mut self, workdir: impl Into<String>) -> Self {
        self.workdir = workdir.into();
        self
    }

    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    pub fn with_stdio_paths(
        mut self,
        stdout_path: impl Into<PathBuf>,
        stderr_path: impl Into<PathBuf>,
    ) -> Self {
        self.stdout_path = Some(stdout_path.into());
        self.stderr_path = Some(stderr_path.into());
        self
    }

    pub fn validate(&self) -> Result<(), SyscallError> {
        if self.argv.is_empty() {
            return Err(SyscallError::Capability(
                "argv must contain the program path".to_string(),
            ));
        }
        if !self.rootfs.is_absolute() {
            return Err(SyscallError::Capability(format!(
                "rootfs must be absolute, got {}",
                self.rootfs.display()
            )));
        }
        if !self.workdir.starts_with('/') {
            return Err(SyscallError::Capability(format!(
                "workdir must be absolute, got {}",
                self.workdir
            )));
        }
        if !self.cgroup_path.is_absolute() {
            return Err(SyscallError::Capability(format!(
                "cgroup_path must be absolute, got {}",
                self.cgroup_path.display()
            )));
        }
        if self.userns && (self.uid_map.is_none() || self.gid_map.is_none()) {
            return Err(SyscallError::Capability(
                "user namespace requires both uid_map and gid_map".to_string(),
            ));
        }
        Ok(())
    }

    pub fn setup_plan(&self) -> Result<NamespaceSetupPlan, SyscallError> {
        self.validate()?;
        Ok(NamespaceSetupPlan {
            clone_flags: self.clone_flags(),
            uid_map: self.uid_map.map(uid_map_line),
            gid_map: self.gid_map.map(uid_map_line),
            cgroup_procs_path: self.cgroup_path.join("cgroup.procs"),
            deny_setgroups: self.gid_map.is_some(),
            mount_proc: self.mount_proc,
            no_new_privs: self.no_new_privs,
            drop_bounding_caps: self.drop_bounding_caps,
        })
    }

    pub fn native_launch_plan(&self) -> Result<NativeLaunchPlan, SyscallError> {
        let setup = self.setup_plan()?;
        let rootfs = path_cstring("rootfs", &self.rootfs)?;
        let workdir = string_cstring("workdir", &self.workdir)?;
        let argv = self
            .argv
            .iter()
            .map(|arg| string_cstring("argv", arg))
            .collect::<Result<Vec<_>, _>>()?;
        let program = argv.first().cloned().ok_or_else(|| {
            SyscallError::Capability("argv must contain the program path".to_string())
        })?;
        let env = self
            .env
            .iter()
            .map(|(key, value)| env_cstring(key, value))
            .collect::<Result<Vec<_>, _>>()?;
        let proc_mount_target = string_cstring("proc mount target", "/proc")?;
        let proc_fs_type = string_cstring("proc fs type", "proc")?;
        let stdout_path = self
            .stdout_path
            .as_deref()
            .map(|path| path_cstring("stdout path", path))
            .transpose()?;
        let stderr_path = self
            .stderr_path
            .as_deref()
            .map(|path| path_cstring("stderr path", path))
            .transpose()?;

        Ok(NativeLaunchPlan {
            setup,
            rootfs,
            workdir,
            program,
            argv,
            env,
            proc_mount_target,
            proc_fs_type,
            stdout_path,
            stderr_path,
        })
    }

    fn clone_flags(&self) -> libc::c_int {
        let mut flags = 0;
        if self.userns {
            flags |= libc::CLONE_NEWUSER;
        }
        if self.pid_ns {
            flags |= libc::CLONE_NEWPID;
        }
        if self.net_ns {
            flags |= libc::CLONE_NEWNET;
        }
        if self.mount_ns {
            flags |= libc::CLONE_NEWNS;
        }
        if self.uts_ns {
            flags |= libc::CLONE_NEWUTS;
        }
        if self.ipc_ns {
            flags |= libc::CLONE_NEWIPC;
        }
        flags
    }
}

fn uid_map_line(map: UidMap) -> String {
    format!("{} {} {}\n", map.inside, map.outside, map.size)
}

fn write_proc_setup_file(
    proc_pid_dir: &Path,
    name: &'static str,
    content: &str,
) -> Result<(), SyscallError> {
    let path = proc_pid_dir.join(name);
    std::fs::write(&path, content).map_err(|error| SyscallError::NamespaceSetup {
        path,
        reason: error.to_string(),
    })
}

fn path_cstring(label: &'static str, path: &Path) -> Result<CString, SyscallError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        SyscallError::Capability(format!(
            "{label} contains an interior NUL byte: {}",
            path.display()
        ))
    })
}

fn string_cstring(label: &'static str, value: &str) -> Result<CString, SyscallError> {
    CString::new(value.as_bytes())
        .map_err(|_| SyscallError::Capability(format!("{label} contains an interior NUL byte")))
}

fn env_cstring(key: &str, value: &str) -> Result<CString, SyscallError> {
    if key.is_empty() {
        return Err(SyscallError::Capability(
            "environment key must not be empty".to_string(),
        ));
    }
    if key.contains('=') {
        return Err(SyscallError::Capability(format!(
            "environment key contains '=': {key}"
        )));
    }
    string_cstring("environment entry", &format!("{key}={value}"))
}

/// Fork + unshare + apply uid_map + exec argv. Returns the child pid.
///
/// **Privileged**: requires `CAP_SYS_ADMIN` (or root) for namespace creation
/// when `userns=false`; usable unprivileged with `userns=true` on kernels
/// with unprivileged userns enabled. Returns `SyscallError::Capability` on
/// non-Linux platforms or when the calling context lacks the required
/// capabilities.
pub fn spawn_namespaced_process(config: &NamespaceConfig) -> Result<u32, SyscallError> {
    let plan = config.native_launch_plan()?;
    let pipes = SyncPipes::new()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let error = std::io::Error::last_os_error();
        pipes.close_all();
        return Err(SyscallError::Io(error));
    }

    if pid == 0 {
        unsafe {
            child_stage1(&plan, &pipes);
        }
    }

    parent_finish_launch(pid as u32, &plan.setup, pipes)
}

fn parent_finish_launch(
    pid: u32,
    setup: &NamespaceSetupPlan,
    pipes: SyncPipes,
) -> Result<u32, SyscallError> {
    pipes.close_child_ends();
    let ready = pipes.read_child_ready();
    if let Err(error) = ready {
        pipes.close_parent_ends();
        return Err(SyscallError::Io(error));
    }

    let setup_result = setup
        .write_id_maps_for_pid(pid)
        .and_then(|()| attach_pid_to_cgroup(pid, &setup.cgroup_procs_path));
    let release_byte = if setup_result.is_ok() { b'1' } else { b'0' };
    let release_result = pipes.write_parent_release(release_byte);
    pipes.close_parent_ends();

    setup_result?;
    release_result.map_err(SyscallError::Io)?;
    Ok(pid)
}

fn attach_pid_to_cgroup(pid: u32, cgroup_procs_path: &Path) -> Result<(), SyscallError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(cgroup_procs_path)
        .map_err(|error| SyscallError::NamespaceSetup {
            path: cgroup_procs_path.to_path_buf(),
            reason: error.to_string(),
        })?;
    writeln!(file, "{pid}").map_err(|error| SyscallError::NamespaceSetup {
        path: cgroup_procs_path.to_path_buf(),
        reason: error.to_string(),
    })
}

struct SyncPipes {
    child_ready_read: RawFd,
    child_ready_write: RawFd,
    parent_release_read: RawFd,
    parent_release_write: RawFd,
}

impl SyncPipes {
    fn new() -> Result<Self, SyscallError> {
        let child_ready = pipe_cloexec()?;
        let parent_release = pipe_cloexec()?;
        Ok(Self {
            child_ready_read: child_ready[0],
            child_ready_write: child_ready[1],
            parent_release_read: parent_release[0],
            parent_release_write: parent_release[1],
        })
    }

    fn close_child_ends(&self) {
        close_fd(self.child_ready_write);
        close_fd(self.parent_release_read);
    }

    fn close_parent_ends(&self) {
        close_fd(self.child_ready_read);
        close_fd(self.parent_release_write);
    }

    fn close_all(&self) {
        close_fd(self.child_ready_read);
        close_fd(self.child_ready_write);
        close_fd(self.parent_release_read);
        close_fd(self.parent_release_write);
    }

    fn read_child_ready(&self) -> std::io::Result<()> {
        read_exact_byte(self.child_ready_read).map(|_| ())
    }

    fn write_parent_release(&self, byte: u8) -> std::io::Result<()> {
        write_byte(self.parent_release_write, byte)
    }
}

fn pipe_cloexec() -> Result<[RawFd; 2], SyscallError> {
    let mut fds = [-1, -1];
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if result < 0 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    Ok(fds)
}

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        let _ = unsafe { libc::close(fd) };
    }
}

fn read_exact_byte(fd: RawFd) -> std::io::Result<u8> {
    let mut byte = 0_u8;
    loop {
        let result = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if result == 1 {
            return Ok(byte);
        }
        if result == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "sync pipe closed",
            ));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

fn write_byte(fd: RawFd, byte: u8) -> std::io::Result<()> {
    loop {
        let result = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
        if result == 1 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

unsafe fn child_stage1(plan: &NativeLaunchPlan, pipes: &SyncPipes) -> ! {
    pipes.close_parent_ends();

    if unsafe { libc::unshare(plan.setup.clone_flags) } < 0 {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    if write_byte(pipes.child_ready_write, b'R').is_err() {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    match read_exact_byte(pipes.parent_release_read) {
        Ok(b'1') => {}
        _ => unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) },
    }
    close_fd(pipes.child_ready_write);
    close_fd(pipes.parent_release_read);

    if plan.setup.clone_flags & libc::CLONE_NEWPID != 0 {
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
        }
        if pid > 0 {
            unsafe { wait_for_stage2(pid) };
        }
    }

    unsafe { child_exec(plan) };
}

unsafe fn wait_for_stage2(pid: libc::pid_t) -> ! {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            if libc::WIFEXITED(status) {
                unsafe { libc::_exit(libc::WEXITSTATUS(status)) };
            }
            if libc::WIFSIGNALED(status) {
                unsafe { libc::_exit(128 + libc::WTERMSIG(status)) };
            }
            unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
}

unsafe fn child_exec(plan: &NativeLaunchPlan) -> ! {
    if plan.setup.clone_flags & libc::CLONE_NEWNS != 0 {
        let propagation = libc::MS_PRIVATE | libc::MS_REC;
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                propagation,
                std::ptr::null(),
            )
        } < 0
        {
            unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
        }
    }

    if let Some(stdout_path) = &plan.stdout_path {
        unsafe { redirect_stdio(stdout_path, libc::STDOUT_FILENO) };
    }
    if let Some(stderr_path) = &plan.stderr_path {
        unsafe { redirect_stdio(stderr_path, libc::STDERR_FILENO) };
    }

    if unsafe { libc::chroot(plan.rootfs.as_ptr()) } < 0 {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    if unsafe { libc::chdir(plan.workdir.as_ptr()) } < 0 {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }

    if plan.setup.mount_proc
        && unsafe {
            libc::mount(
                plan.proc_fs_type.as_ptr(),
                plan.proc_mount_target.as_ptr(),
                plan.proc_fs_type.as_ptr(),
                (libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV) as libc::c_ulong,
                std::ptr::null(),
            )
        } < 0
    {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }

    if plan.setup.no_new_privs && unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0
    {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    if plan.setup.drop_bounding_caps {
        for capability in 0..=CAP_LAST_CAP {
            if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) } < 0 {
                unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
            }
        }
    }

    let mut argv = plan.argv.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
    argv.push(std::ptr::null());
    let mut env = plan
        .env
        .iter()
        .map(|entry| entry.as_ptr())
        .collect::<Vec<_>>();
    env.push(std::ptr::null());

    unsafe { libc::execve(plan.program.as_ptr(), argv.as_ptr(), env.as_ptr()) };
    unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
}

unsafe fn redirect_stdio(path: &std::ffi::CStr, target_fd: libc::c_int) {
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    if unsafe { libc::dup2(fd, target_fd) } < 0 {
        let _ = unsafe { libc::close(fd) };
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    let _ = unsafe { libc::close(fd) };
}

const CHILD_SETUP_EXIT_CODE: i32 = 127;
const CAP_LAST_CAP: libc::c_ulong = 40;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_enables_full_isolation() {
        let cfg = NamespaceConfig::default();
        assert!(cfg.userns);
        assert!(cfg.pid_ns);
        assert!(cfg.net_ns);
        assert!(cfg.mount_ns);
        assert!(cfg.no_new_privs);
        assert!(cfg.drop_bounding_caps);
    }

    #[test]
    fn with_uid_map_sets_both_uid_and_gid() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");
        assert_eq!(
            cfg.uid_map,
            Some(UidMap {
                inside: 0,
                outside: 100000,
                size: 65536,
            })
        );
        assert_eq!(
            cfg.gid_map,
            Some(UidMap {
                inside: 0,
                outside: 100000,
                size: 65536
            })
        );
    }

    #[test]
    fn setup_plan_builds_linux_clone_flags_and_map_payloads() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");

        let plan = cfg.setup_plan().expect("setup plan");

        assert_eq!(
            plan.clone_flags,
            libc::CLONE_NEWUSER
                | libc::CLONE_NEWPID
                | libc::CLONE_NEWNET
                | libc::CLONE_NEWNS
                | libc::CLONE_NEWUTS
                | libc::CLONE_NEWIPC
        );
        assert_eq!(plan.uid_map.as_deref(), Some("0 100000 65536\n"));
        assert_eq!(plan.gid_map.as_deref(), Some("0 100000 65536\n"));
        assert_eq!(
            plan.cgroup_procs_path,
            PathBuf::from("/sys/fs/cgroup/denia/test/cgroup.procs")
        );
        assert!(plan.deny_setgroups);
        assert!(plan.mount_proc);
        assert!(plan.no_new_privs);
        assert!(plan.drop_bounding_caps);
    }

    #[test]
    fn setup_plan_writes_setgroups_before_id_maps() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");
        let plan = cfg.setup_plan().expect("setup plan");
        let proc_pid = tempfile::tempdir().expect("proc pid dir");

        plan.write_id_maps_at(proc_pid.path()).expect("id maps");

        assert_eq!(
            std::fs::read_to_string(proc_pid.path().join("setgroups")).expect("setgroups"),
            "deny\n"
        );
        assert_eq!(
            std::fs::read_to_string(proc_pid.path().join("uid_map")).expect("uid_map"),
            "0 100000 65536\n"
        );
        assert_eq!(
            std::fs::read_to_string(proc_pid.path().join("gid_map")).expect("gid_map"),
            "0 100000 65536\n"
        );
    }

    #[test]
    fn setup_plan_reports_id_map_write_errors_with_path() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");
        let plan = cfg.setup_plan().expect("setup plan");
        let missing_proc_pid = tempfile::tempdir()
            .expect("proc pid parent")
            .path()
            .join("missing");

        let error = plan
            .write_id_maps_at(&missing_proc_pid)
            .expect_err("missing proc pid dir");

        assert!(
            matches!(error, SyscallError::NamespaceSetup { ref path, .. } if path == &missing_proc_pid.join("setgroups")),
            "expected namespace setup path, got: {error:?}"
        );
    }

    #[test]
    fn native_launch_plan_materializes_c_compatible_payloads() {
        let cfg = NamespaceConfig::new(
            "/var/lib/denia/rootfs",
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo ok".to_string(),
            ],
        )
        .with_uid_map(100000, 65536)
        .with_cgroup_path("/sys/fs/cgroup/denia/test")
        .with_workdir("/srv/app")
        .with_env(vec![
            (
                "DENIA_SERVICE_SOCKET".to_string(),
                "/run/denia/service.sock".to_string(),
            ),
            ("PORT".to_string(), "3000".to_string()),
        ]);

        let plan = cfg.native_launch_plan().expect("native launch plan");

        assert_eq!(plan.program.to_str().expect("program"), "/bin/sh");
        assert_eq!(
            plan.rootfs.to_str().expect("rootfs"),
            "/var/lib/denia/rootfs"
        );
        assert_eq!(plan.workdir.to_str().expect("workdir"), "/srv/app");
        assert_eq!(plan.proc_mount_target.to_str().expect("proc"), "/proc");
        assert_eq!(plan.proc_fs_type.to_str().expect("proc fs"), "proc");
        assert_eq!(plan.stdout_path, None);
        assert_eq!(plan.stderr_path, None);
        assert_eq!(
            plan.argv
                .iter()
                .map(|arg| arg.to_str().expect("arg").to_string())
                .collect::<Vec<_>>(),
            vec!["/bin/sh", "-c", "echo ok"]
        );
        assert_eq!(
            plan.env
                .iter()
                .map(|entry| entry.to_str().expect("env").to_string())
                .collect::<Vec<_>>(),
            vec!["DENIA_SERVICE_SOCKET=/run/denia/service.sock", "PORT=3000"]
        );
        assert_eq!(
            plan.setup.cgroup_procs_path,
            PathBuf::from("/sys/fs/cgroup/denia/test/cgroup.procs")
        );
    }

    #[test]
    fn native_launch_plan_materializes_stdio_paths() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_stdio_paths("/var/log/denia/stdout.log", "/var/log/denia/stderr.log");

        let plan = cfg.native_launch_plan().expect("native launch plan");

        assert_eq!(
            plan.stdout_path
                .as_ref()
                .expect("stdout")
                .to_str()
                .expect("stdout path"),
            "/var/log/denia/stdout.log"
        );
        assert_eq!(
            plan.stderr_path
                .as_ref()
                .expect("stderr")
                .to_str()
                .expect("stderr path"),
            "/var/log/denia/stderr.log"
        );
    }

    #[test]
    fn native_launch_plan_rejects_nul_bytes_before_fork() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true\0bad".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");

        let error = cfg
            .native_launch_plan()
            .expect_err("nul argv must be rejected");

        assert!(
            matches!(error, SyscallError::Capability(ref reason) if reason.contains("argv contains an interior NUL byte")),
            "expected argv nul error, got: {error:?}"
        );
    }

    #[test]
    fn native_launch_plan_rejects_invalid_env_keys_before_fork() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_env(vec![("BAD=KEY".to_string(), "value".to_string())]);

        let error = cfg
            .native_launch_plan()
            .expect_err("env key must be rejected");

        assert!(
            matches!(error, SyscallError::Capability(ref reason) if reason.contains("environment key contains '='")),
            "expected env key error, got: {error:?}"
        );
    }

    #[test]
    fn validate_rejects_empty_argv() {
        let cfg = NamespaceConfig {
            rootfs: "/var/lib/denia/rootfs".into(),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_relative_rootfs() {
        let cfg = NamespaceConfig::new("relative/path", vec!["/bin/true".to_string()]);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_userns_without_id_maps() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()])
            .with_cgroup_path("/sys/fs/cgroup/denia/test");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_relative_cgroup_path() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("relative/cgroup");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn spawn_rejects_invalid_payload_before_fork() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true\0bad".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");
        let err = spawn_namespaced_process(&cfg).unwrap_err();
        match err {
            SyscallError::Capability(reason) => {
                assert!(reason.contains("argv contains an interior NUL byte"));
            }
            other => panic!("expected Capability, got {other:?}"),
        };
    }
}
