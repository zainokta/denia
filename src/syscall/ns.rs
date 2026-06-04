use std::{
    ffi::CString,
    os::fd::RawFd,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use rustix::process::Signal;

use crate::syscall::{SyscallError, caps, seccomp, signal};

/// Configuration for a namespaced one-shot or long-running process launch.
///
/// `NamespaceConfig` describes the Linux namespaces, id maps, rootfs, cgroup,
/// stdio, and exec payload used by Denia's native process runner.
///
/// The struct is intentionally a builder around concrete fields so its public
/// surface is testable without needing root privileges. Privileged execution
/// is gated to `spawn_namespaced_process`; callers must run that path only in
/// the privileged runtime context because namespace, cgroup, mount, and chroot
/// setup require host-root capabilities.
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
    pub mask_proc: bool,
    pub setup_dev: bool,
    pub seccomp: bool,
    pub max_pids: Option<u64>,
    pub max_fds: Option<u64>,
    pub max_procs: Option<u64>,
    /// Optional private overlay filesystem to use as the new root.
    ///
    /// When set, `child_exec` mounts an overlay (shared read-only `lower`,
    /// per-replica writable `upper`/`work`) at `merged` and pivots into it
    /// instead of bind-mounting `rootfs` onto itself. Used by autoscaling so
    /// each replica gets an isolated writable layer over the shared artifact
    /// rootfs.
    pub overlay: Option<OverlaySpec>,
    /// Read-only bind mounts (e.g. helper binaries) injected into the new root
    /// after `pivot_root`, before proc is mounted.
    pub ro_binds: Vec<RoBind>,
    /// Optional read-WRITE bind of a host directory onto an absolute guest path,
    /// applied pre-userns. Used for the per-replica socket dir so the workload's
    /// unix socket lives on the real host fs (the same inode the daemon connects
    /// to) instead of in the overlay — an AF_UNIX socket created on overlayfs is
    /// bound to the overlay inode and is NOT connectable via the upperdir path.
    /// `(host_src, guest_dest)`.
    pub socket_bind: Option<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidMap {
    pub inside: u32,
    pub outside: u32,
    pub size: u32,
}

/// Private overlay filesystem for a replica's root.
///
/// `lower` is the shared read-only artifact rootfs; `upper` and `work` are the
/// per-replica writable layer and overlay workdir; `merged` is the mountpoint
/// that becomes the new root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlaySpec {
    pub lower: PathBuf,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub merged: PathBuf,
}

/// A read-only bind mount injected into the guest root.
///
/// `src` is the host source path; `dest` is an absolute path inside the new
/// root (e.g. `/.denia/socket-proxy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoBind {
    pub src: PathBuf,
    pub dest: PathBuf,
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
    pub mask_proc: bool,
    pub setup_dev: bool,
    pub seccomp: bool,
    pub max_pids: Option<u64>,
    pub max_fds: Option<u64>,
    pub max_procs: Option<u64>,
    pub overlay: Option<OverlayPlan>,
    pub ro_binds: Vec<RoBindPlan>,
    pub socket_bind: Option<(CString, CString)>,
}

/// CString-ized overlay paths and mount data for the post-fork child.
///
/// All allocation happens in the parent so the post-fork child never allocates
/// fallibly. `overlay_fs_type` is the literal `"overlay"` filesystem name and
/// `data` is the `lowerdir=...,upperdir=...,workdir=...` mount option string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPlan {
    pub lower: CString,
    pub upper: CString,
    pub work: CString,
    pub merged: CString,
    pub overlay_fs_type: CString,
    pub data: CString,
}

/// CString-ized read-only bind mount for the post-fork child.
///
/// `dest` is absolute within the new root. `dest_is_file` selects whether the
/// child creates an empty file (true) or a directory (false) as the mountpoint
/// before binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoBindPlan {
    pub src: CString,
    pub dest: CString,
    pub dest_is_file: bool,
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
            mask_proc: true,
            setup_dev: true,
            seccomp: true,
            max_pids: Some(1024),
            max_fds: Some(65536),
            max_procs: Some(1024),
            overlay: None,
            ro_binds: Vec::new(),
            socket_bind: None,
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

    /// Read-write bind a host directory onto `dest` (absolute within the new
    /// root). Used for the per-replica socket dir; see the `socket_bind` field.
    pub fn with_socket_bind(mut self, src: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        self.socket_bind = Some((src.into(), dest.into()));
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

    pub fn with_overlay(mut self, overlay: OverlaySpec) -> Self {
        self.overlay = Some(overlay);
        self
    }

    pub fn with_ro_bind(mut self, ro_bind: RoBind) -> Self {
        self.ro_binds.push(ro_bind);
        self
    }

    pub fn with_deferred_hardening(mut self) -> Self {
        self.no_new_privs = false;
        self.drop_bounding_caps = false;
        self.seccomp = false;
        self
    }

    pub fn with_mask_proc(mut self, mask: bool) -> Self {
        self.mask_proc = mask;
        self
    }

    pub fn with_setup_dev(mut self, setup: bool) -> Self {
        self.setup_dev = setup;
        self
    }

    pub fn with_seccomp(mut self, enable: bool) -> Self {
        self.seccomp = enable;
        self
    }

    pub fn with_max_pids(mut self, max: Option<u64>) -> Self {
        self.max_pids = max;
        self
    }

    pub fn with_max_fds(mut self, max: Option<u64>) -> Self {
        self.max_fds = max;
        self
    }

    pub fn with_max_procs(mut self, max: Option<u64>) -> Self {
        self.max_procs = max;
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
        if !is_safe_absolute_workdir(&self.workdir) {
            return Err(SyscallError::Capability(format!(
                "workdir must be absolute and normalized within root, got {}",
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
        let overlay = self.overlay.as_ref().map(overlay_plan).transpose()?;
        let ro_binds = self
            .ro_binds
            .iter()
            .map(ro_bind_plan)
            .collect::<Result<Vec<_>, _>>()?;
        let socket_bind = match &self.socket_bind {
            Some((src, dest)) => Some((
                path_cstring("socket bind src", src)?,
                path_cstring("socket bind dest", dest)?,
            )),
            None => None,
        };

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
            mask_proc: self.mask_proc,
            setup_dev: self.setup_dev,
            seccomp: self.seccomp,
            max_pids: self.max_pids,
            max_fds: self.max_fds,
            max_procs: self.max_procs,
            overlay,
            ro_binds,
            socket_bind,
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

fn is_safe_absolute_workdir(workdir: &str) -> bool {
    if workdir.as_bytes().contains(&0) {
        return false;
    }
    if workdir
        .split('/')
        .skip(1)
        .any(|component| matches!(component, "." | ".."))
    {
        return false;
    }
    let mut components = Path::new(workdir).components();
    if !matches!(components.next(), Some(std::path::Component::RootDir)) {
        return false;
    }
    components.all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn uid_map_line(map: UidMap) -> String {
    format!("{} {} {}\n", map.inside, map.outside, map.size)
}

fn overlay_plan(spec: &OverlaySpec) -> Result<OverlayPlan, SyscallError> {
    let lower = path_cstring("overlay lower", &spec.lower)?;
    let upper = path_cstring("overlay upper", &spec.upper)?;
    let work = path_cstring("overlay work", &spec.work)?;
    let merged = path_cstring("overlay merged", &spec.merged)?;
    let overlay_fs_type = string_cstring("overlay fs type", "overlay")?;
    let data = overlay_data_cstring(&spec.lower, &spec.upper, &spec.work)?;
    Ok(OverlayPlan {
        lower,
        upper,
        work,
        merged,
        overlay_fs_type,
        data,
    })
}

fn overlay_data_cstring(lower: &Path, upper: &Path, work: &Path) -> Result<CString, SyscallError> {
    reject_overlay_separator("overlay lower", lower)?;
    reject_overlay_separator("overlay upper", upper)?;
    reject_overlay_separator("overlay work", work)?;
    let mut data = Vec::new();
    data.extend_from_slice(b"lowerdir=");
    data.extend_from_slice(lower.as_os_str().as_bytes());
    data.extend_from_slice(b",upperdir=");
    data.extend_from_slice(upper.as_os_str().as_bytes());
    data.extend_from_slice(b",workdir=");
    data.extend_from_slice(work.as_os_str().as_bytes());
    // No `userxattr`: the overlay is mounted privileged in the initial user
    // namespace (see `child_prepare_root` / ADR-026), so it uses the standard
    // `trusted.overlay.*` xattr namespace. Mounting it inside the workload's
    // unprivileged userns instead fails EACCES on btrfs ("upper fs does not
    // support tmpfile").
    CString::new(data).map_err(|_| {
        SyscallError::Capability("overlay mount data contains an interior NUL byte".to_string())
    })
}

fn reject_overlay_separator(label: &'static str, path: &Path) -> Result<(), SyscallError> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&b',') || bytes.contains(&b':') {
        return Err(SyscallError::Capability(format!(
            "{label} must not contain ',' or ':' overlay option separators: {}",
            path.display()
        )));
    }
    Ok(())
}

fn ro_bind_plan(bind: &RoBind) -> Result<RoBindPlan, SyscallError> {
    if !bind.dest.is_absolute() {
        return Err(SyscallError::Capability(format!(
            "ro bind dest must be absolute, got {}",
            bind.dest.display()
        )));
    }
    let src = path_cstring("ro bind src", &bind.src)?;
    let dest = path_cstring("ro bind dest", &bind.dest)?;
    // A file source is bound onto a file mountpoint; a directory source needs a
    // directory mountpoint. The decision is made here in the parent so the
    // post-fork child only performs the corresponding mkdir/creat.
    let dest_is_file = bind.src.is_file();
    Ok(RoBindPlan {
        src,
        dest,
        dest_is_file,
    })
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
    let argv_ptrs = null_terminated_ptrs(&plan.argv);
    let env_ptrs = null_terminated_ptrs(&plan.env);
    let pipes = SyncPipes::new()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let error = std::io::Error::last_os_error();
        pipes.close_all();
        return Err(SyscallError::Io(error));
    }

    if pid == 0 {
        unsafe {
            child_stage1(&plan, &pipes, &argv_ptrs, &env_ptrs);
        }
    }

    parent_finish_launch(pid as u32, &plan.setup, pipes)
}

fn null_terminated_ptrs(strings: &[CString]) -> Vec<*const libc::c_char> {
    let mut ptrs = strings
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    ptrs.push(std::ptr::null());
    ptrs
}

fn parent_finish_launch(
    pid: u32,
    setup: &NamespaceSetupPlan,
    pipes: SyncPipes,
) -> Result<u32, SyscallError> {
    pipes.close_child_ends();
    if let Err(error) = pipes.read_child_ready("initial child ready") {
        return abort_launch(pid, pipes, error);
    }

    let cgroup_result = attach_pid_to_cgroup(pid, &setup.cgroup_procs_path);
    let release_byte = if cgroup_result.is_ok() { b'1' } else { b'0' };
    let release_result = pipes.write_parent_release(release_byte);
    if let Err(error) = cgroup_result {
        return abort_launch(pid, pipes, error);
    }
    if let Err(error) = release_result {
        return abort_launch(pid, pipes, SyscallError::Io(error));
    }

    if let Err(error) = pipes.read_child_ready("post-unshare child ready") {
        return abort_launch(pid, pipes, error);
    }

    let id_map_result = setup.write_id_maps_for_pid(pid);
    let release_byte = if id_map_result.is_ok() { b'1' } else { b'0' };
    let release_result = pipes.write_parent_release(release_byte);
    if let Err(error) = release_result {
        return abort_launch(pid, pipes, SyscallError::Io(error));
    }
    let child_setup_status = pipes.read_child_setup_status();
    if let Err(error) = id_map_result {
        return abort_launch(pid, pipes, error);
    }
    if let Err(error) = child_setup_status {
        return abort_launch(pid, pipes, error);
    }
    pipes.close_parent_ends();
    Ok(pid)
}

fn abort_launch(pid: u32, pipes: SyncPipes, error: SyscallError) -> Result<u32, SyscallError> {
    pipes.close_parent_ends();
    let _ = signal::kill(pid, Signal::KILL);
    let _ = signal::wait(pid);
    Err(error)
}

fn attach_pid_to_cgroup(pid: u32, cgroup_procs_path: &Path) -> Result<(), SyscallError> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(cgroup_procs_path)
        .map_err(|error| SyscallError::NamespaceSetup {
            path: cgroup_procs_path.to_path_buf(),
            reason: format!("open: {error}"),
        })?;
    tracing::debug!(
        pid,
        target = %cgroup_procs_path.display(),
        "attaching workload pid to cgroup"
    );
    // Must be a SINGLE write(2) — `writeln!`/`write_fmt` can emit the digits
    // and the newline as two separate writes, which the kernel's
    // `cgroup.procs` handler parses as two writes: the first migrates the
    // pid; the second sees an empty payload and returns EINVAL. Build the
    // payload first, then issue one write_all of the complete bytes.
    let payload = format!("{pid}\n");
    file.write_all(payload.as_bytes())
        .map_err(|error| SyscallError::NamespaceSetup {
            path: cgroup_procs_path.to_path_buf(),
            reason: format!("write pid={pid}: {error}"),
        })
}

struct SyncPipes {
    child_ready_read: RawFd,
    child_ready_write: RawFd,
    parent_release_read: RawFd,
    parent_release_write: RawFd,
    child_error_read: RawFd,
    child_error_write: RawFd,
}

impl SyncPipes {
    fn new() -> Result<Self, SyscallError> {
        let child_ready = pipe_cloexec()?;
        let parent_release = pipe_cloexec()?;
        let child_error = pipe_cloexec()?;
        Ok(Self {
            child_ready_read: child_ready[0],
            child_ready_write: child_ready[1],
            parent_release_read: parent_release[0],
            parent_release_write: parent_release[1],
            child_error_read: child_error[0],
            child_error_write: child_error[1],
        })
    }

    fn close_child_ends(&self) {
        close_fd(self.child_ready_write);
        close_fd(self.parent_release_read);
        close_fd(self.child_error_write);
    }

    fn close_parent_ends(&self) {
        close_fd(self.child_ready_read);
        close_fd(self.parent_release_write);
        close_fd(self.child_error_read);
    }

    fn close_all(&self) {
        close_fd(self.child_ready_read);
        close_fd(self.child_ready_write);
        close_fd(self.parent_release_read);
        close_fd(self.parent_release_write);
        close_fd(self.child_error_read);
        close_fd(self.child_error_write);
    }

    fn read_child_ready(&self, stage: &'static str) -> Result<(), SyscallError> {
        match read_exact_byte_timeout(self.child_ready_read, CHILD_SETUP_TIMEOUT) {
            TimedByte::Byte(_) => Ok(()),
            TimedByte::Eof => Err(SyscallError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "sync pipe closed",
            ))),
            TimedByte::Timeout => Err(SyscallError::ChildSetupTimeout { stage }),
            TimedByte::Error(error) => Err(SyscallError::Io(error)),
        }
    }

    fn write_parent_release(&self, byte: u8) -> std::io::Result<()> {
        write_byte(self.parent_release_write, byte)
    }

    fn read_child_setup_status(&self) -> Result<(), SyscallError> {
        match read_optional_byte_timeout(self.child_error_read, CHILD_SETUP_TIMEOUT) {
            TimedByte::Byte(stage) => {
                // Best-effort drain of the errno follow-up bytes child_setup_fail
                // also writes. Failures here just leave errno unknown.
                let mut errno_bytes = [0u8; 4];
                let mut filled = 0;
                while filled < 4 {
                    let n = unsafe {
                        libc::read(
                            self.child_error_read,
                            errno_bytes.as_mut_ptr().add(filled).cast(),
                            4 - filled,
                        )
                    };
                    if n <= 0 {
                        break;
                    }
                    filled += n as usize;
                }
                let errno = if filled == 4 {
                    Some(i32::from_le_bytes(errno_bytes))
                } else {
                    None
                };
                let stage_str = child_setup_stage(stage);
                let stage_with_errno = match errno {
                    Some(e) => Box::leak(
                        format!("{stage_str} errno={e} ({})", errno_str(e)).into_boxed_str(),
                    ),
                    None => stage_str,
                };
                Err(SyscallError::ChildSetup {
                    stage: stage_with_errno,
                })
            }
            TimedByte::Eof => Ok(()),
            TimedByte::Timeout => Err(SyscallError::ChildSetupTimeout {
                stage: "child setup status",
            }),
            TimedByte::Error(error) => Err(SyscallError::Io(error)),
        }
    }
}

fn errno_str(e: i32) -> &'static str {
    match e {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::EIO => "EIO",
        libc::ENOEXEC => "ENOEXEC",
        libc::EBADF => "EBADF",
        libc::EACCES => "EACCES",
        libc::EFAULT => "EFAULT",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::ENODEV => "ENODEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENOSPC => "ENOSPC",
        libc::EROFS => "EROFS",
        libc::ELOOP => "ELOOP",
        libc::ENOTSUP => "ENOTSUP",
        _ => "?",
    }
}

fn pipe_cloexec() -> Result<[RawFd; 2], SyscallError> {
    use rustix::fd::IntoRawFd;
    use rustix::pipe::{PipeFlags, pipe_with};
    let (reader, writer) = pipe_with(PipeFlags::CLOEXEC).map_err(|e| SyscallError::Io(e.into()))?;
    Ok([reader.into_raw_fd(), writer.into_raw_fd()])
}

fn close_fd(fd: RawFd) {
    if fd >= 0 {
        let _ = unsafe { libc::close(fd) };
    }
}

enum TimedByte {
    Byte(u8),
    Eof,
    Timeout,
    Error(std::io::Error),
}

fn read_exact_byte_timeout(fd: RawFd, timeout: Duration) -> TimedByte {
    match wait_fd_readable(fd, timeout) {
        Ok(true) => read_byte_now(fd),
        Ok(false) => TimedByte::Timeout,
        Err(error) => TimedByte::Error(error),
    }
}

fn read_optional_byte_timeout(fd: RawFd, timeout: Duration) -> TimedByte {
    read_exact_byte_timeout(fd, timeout)
}

fn wait_fd_readable(fd: RawFd, timeout: Duration) -> std::io::Result<bool> {
    let timeout_ms = timeout.as_millis().try_into().unwrap_or(i32::MAX);
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if result > 0 {
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

fn read_byte_now(fd: RawFd) -> TimedByte {
    let mut byte = 0_u8;
    loop {
        let result = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if result == 1 {
            return TimedByte::Byte(byte);
        }
        if result == 0 {
            return TimedByte::Eof;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return TimedByte::Error(error);
    }
}

fn read_exact_byte(fd: RawFd) -> std::io::Result<u8> {
    match read_byte_now(fd) {
        TimedByte::Byte(byte) => Ok(byte),
        TimedByte::Eof => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "sync pipe closed",
        )),
        // `read_byte_now` never returns `Timeout` (only `read_*_byte_timeout`
        // does, via the poll path), so this arm is not reachable today. Return a
        // typed error rather than panicking on the launch path, per the
        // "no panics for expected failures" rule.
        TimedByte::Timeout => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "sync pipe read timed out",
        )),
        TimedByte::Error(error) => Err(error),
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

fn child_setup_stage(stage: u8) -> &'static str {
    match stage {
        b'C' => "cgroup release",
        b'U' => "unshare mount namespace",
        b'X' => "unshare user/pid namespace",
        b'M' => "id-map release",
        b'F' => "pid namespace fork",
        b'P' => "make mount propagation private",
        b'O' => "open stdio",
        b'R' => "chroot",
        b'a' => "chroot: overlay mount",
        b'r' => "chroot: new-root self-bind mount",
        b'o' => "chroot: create /.old_root",
        b'v' => "chroot: pivot_root",
        b'u' => "chroot: unmount /.old_root",
        b'b' => "read-only bind mount",
        b'W' => "chdir workdir",
        b'p' => "mount proc",
        b'D' => "setup /dev tmpfs",
        b'm' => "mask /proc paths",
        b'n' => "bring loopback up",
        b'k' => "socket dir bind mount",
        b'g' => "setgid userns root",
        b'i' => "setuid userns root",
        b'N' => "set no_new_privs",
        b'B' => "drop capability bounding set",
        b'L' => "apply resource limits",
        b'S' => "install seccomp filter",
        b'E' => "execve",
        _ => "unknown setup stage",
    }
}

#[repr(C)]
struct IfReqFlags {
    ifr_name: [libc::c_char; libc::IFNAMSIZ],
    ifr_flags: libc::c_short,
    _pad: [u8; 22],
}

/// Bring the loopback interface up in the current network namespace. Requires
/// `CAP_NET_ADMIN` over the user namespace owning the netns; call it before
/// `execve` (which strips the workload's capabilities). Returns the errno on
/// failure. Async-signal-safe (only raw socket/ioctl/close syscalls).
unsafe fn bring_loopback_up() -> Result<(), i32> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(unsafe { *libc::__errno_location() });
    }
    let mut req = IfReqFlags {
        ifr_name: [0; libc::IFNAMSIZ],
        ifr_flags: 0,
        _pad: [0; 22],
    };
    req.ifr_name[0] = b'l' as libc::c_char;
    req.ifr_name[1] = b'o' as libc::c_char;
    let result = unsafe {
        if libc::ioctl(sock, libc::SIOCGIFFLAGS, &mut req) < 0 {
            Err(*libc::__errno_location())
        } else {
            req.ifr_flags |= libc::IFF_UP as libc::c_short;
            if libc::ioctl(sock, libc::SIOCSIFFLAGS, &req) < 0 {
                Err(*libc::__errno_location())
            } else {
                Ok(())
            }
        }
    };
    unsafe { libc::close(sock) };
    result
}

unsafe fn child_setup_fail(pipes: &SyncPipes, stage: u8) -> ! {
    // Capture errno BEFORE any further syscall (write_byte uses libc::write).
    let errno = unsafe { *libc::__errno_location() };
    unsafe { child_setup_fail_errno(pipes, stage, errno) }
}

/// Like `child_setup_fail`, but reports an explicit errno. Used for stages whose
/// syscall wrapper (e.g. `rustix`) returns the error by value and does NOT set
/// the C `errno` global — reading `__errno_location()` there yields a stale,
/// misleading value, so the caller passes `Errno::raw_os_error()` directly.
unsafe fn child_setup_fail_errno(pipes: &SyncPipes, stage: u8, errno: i32) -> ! {
    // Pre-format a single 5-byte payload [stage, e0, e1, e2, e3] so the parent
    // can read errno alongside the stage tag without changing the
    // single-byte-then-EOF protocol elsewhere. This is async-signal-safe.
    let bytes = errno.to_le_bytes();
    let payload = [stage, bytes[0], bytes[1], bytes[2], bytes[3]];
    let _ = unsafe {
        libc::write(
            pipes.child_error_write,
            payload.as_ptr().cast(),
            payload.len(),
        )
    };
    unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
}

unsafe fn child_stage1(
    plan: &NativeLaunchPlan,
    pipes: &SyncPipes,
    argv: &[*const libc::c_char],
    env: &[*const libc::c_char],
) -> ! {
    pipes.close_parent_ends();

    let stdio = open_child_stdio(plan, pipes);

    if write_byte(pipes.child_ready_write, b'C').is_err() {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    match read_exact_byte(pipes.parent_release_read) {
        Ok(b'1') => {}
        _ => unsafe { child_setup_fail(pipes, b'C') },
    }

    // Two-stage unshare. The mount namespace is created FIRST, while the child
    // still holds the daemon's initial-namespace CAP_SYS_ADMIN (capabilities
    // survive fork() until CLONE_NEWUSER), and the new root — plus its read-only
    // bind mounts — are set up there, privileged. Mounting overlayfs-as-upper
    // inside an unprivileged user namespace fails EACCES on btrfs ("upper fs does
    // not support tmpfile"); read-only binds fail EACCES too, because the
    // workload's mapped uid cannot traverse the host-absolute bind source (e.g. a
    // 0700 home) and the remount-ro needs CAP_SYS_ADMIN over the host superblock.
    // A privileged, pre-userns setup avoids both. The second unshare re-applies
    // CLONE_NEWNS so the mount namespace carrying the new root is owned by the new
    // user namespace, letting the child pivot_root with CAP_SYS_ADMIN over its own
    // user ns. See ADR-026.
    if plan.setup.clone_flags & libc::CLONE_NEWNS != 0
        && (plan.overlay.is_some()
            || !plan.ro_binds.is_empty()
            || plan.setup_dev
            || plan.socket_bind.is_some())
    {
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } < 0 {
            unsafe { child_setup_fail(pipes, b'U') };
        }
        unsafe { child_prepare_root(plan, pipes) };
    }

    if unsafe { libc::unshare(plan.setup.clone_flags) } < 0 {
        unsafe { child_setup_fail(pipes, b'X') };
    }
    if write_byte(pipes.child_ready_write, b'R').is_err() {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    }
    match read_exact_byte(pipes.parent_release_read) {
        Ok(b'1') => {}
        _ => unsafe { child_setup_fail(pipes, b'M') },
    }
    close_fd(pipes.child_ready_write);
    close_fd(pipes.parent_release_read);

    if plan.setup.clone_flags & libc::CLONE_NEWPID != 0 {
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe { child_setup_fail(pipes, b'F') };
        }
        if pid > 0 {
            close_fd(pipes.child_error_write);
            wait_for_stage2(pid);
        }
    }

    unsafe { child_exec(plan, pipes, argv, env, &stdio) };
}

fn wait_for_stage2(pid: libc::pid_t) -> ! {
    // The caller only invokes this on the `pid > 0` fork branch, so `pid` is
    // always a valid non-zero child pid and `from_raw` always returns `Some`.
    // Exit cleanly rather than panicking in this intermediate process if that
    // invariant is ever violated (avoids unwinding a forked process).
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
    };
    loop {
        match rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()) {
            Ok(Some((_, status))) => {
                if let Some(code) = status.exit_status() {
                    unsafe { libc::_exit(code) };
                }
                if let Some(sig) = status.terminating_signal() {
                    unsafe { libc::_exit(128 + sig) };
                }
                unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) };
            }
            Err(rustix::io::Errno::INTR) => continue,
            _ => unsafe { libc::_exit(CHILD_SETUP_EXIT_CODE) },
        }
    }
}

/// Prepare the new root in the freshly created mount namespace, while the child
/// is still privileged in the initial user namespace. Called between the two
/// unshares in `child_stage1` when an overlay is configured or read-only binds
/// are requested.
///
/// The mount tree is set `MS_REC | MS_PRIVATE` first so neither the overlay nor
/// the read-only binds propagate back into the host's shared mount table
/// (`MountFlags=shared`). Mounting overlayfs-as-upper here — rather than inside
/// the workload's user namespace — is what makes it work on btrfs; the in-userns
/// mount fails EACCES ("upper fs does not support tmpfile"). Read-only binds are
/// applied here for the same reason: the host-absolute bind source may sit under
/// a directory the workload's mapped uid cannot traverse (e.g. a 0700 home), and
/// the remount-ro needs CAP_SYS_ADMIN over the host superblock — both hold
/// pre-userns. The resulting mounts are inherited — and thus `MNT_LOCKED` — by the
/// user namespace created in stage 2, so `child_exec` re-binds the root before
/// `pivot_root` (a locked mount cannot be a `pivot_root` target); the recursive
/// self-bind carries the read-only binds along. See ADR-026.
unsafe fn child_prepare_root(plan: &NativeLaunchPlan, pipes: &SyncPipes) {
    if let Err(e) = rustix::mount::mount_change(
        c"/",
        rustix::mount::MountPropagationFlags::PRIVATE | rustix::mount::MountPropagationFlags::REC,
    ) {
        unsafe { child_setup_fail_errno(pipes, b'P', e.raw_os_error()) };
    }

    // The new root is either a freshly mounted overlay (its `merged` dir) or, for
    // the non-overlay path, the plain artifact rootfs. Read-only binds are placed
    // relative to that base so the recursive self-bind + pivot_root in
    // `child_exec` carry them into the workload.
    let base: &std::ffi::CStr = if let Some(overlay) = &plan.overlay {
        if unsafe {
            libc::mount(
                overlay.overlay_fs_type.as_ptr(),
                overlay.merged.as_ptr(),
                overlay.overlay_fs_type.as_ptr(),
                0,
                overlay.data.as_ptr().cast(),
            )
        } < 0
        {
            unsafe { child_setup_fail(pipes, b'a') }; // overlay mount
        }
        overlay.merged.as_c_str()
    } else {
        plan.rootfs.as_c_str()
    };

    for bind in &plan.ro_binds {
        unsafe { child_apply_ro_bind(pipes, base, bind) };
    }

    // A fresh /dev tmpfs with real device nodes bound from the host. `mknod` is
    // EPERM in the (later) unprivileged userns, and the systemd daemon lacks
    // CAP_MKNOD even here, so we BIND host nodes (needs only CAP_SYS_ADMIN). The
    // recursive self-bind + pivot_root in `child_exec` carry them into the
    // workload. See ADR-026.
    if plan.setup_dev {
        unsafe { child_setup_dev(pipes, base) };
    }

    // Read-write bind the per-replica socket dir from the host onto `dest`, so the
    // workload's unix socket lives on real host fs (the inode the daemon connects
    // to). An AF_UNIX socket created on overlayfs binds to the overlay inode and
    // is NOT connectable via the upperdir path. See ADR-026.
    if let Some((src, dest)) = &plan.socket_bind {
        unsafe { child_bind_dir_rw(pipes, base, dest, src) };
    }
}

/// Read-write bind `src` (a host directory) onto `<base><dest>` (dest absolute
/// within the new root), creating the mountpoint chain. Used for the per-replica
/// socket dir so the workload's unix socket is on real host fs. Runs privileged,
/// pre-userns; the recursive self-bind + pivot_root carry it into the workload.
unsafe fn child_bind_dir_rw(
    pipes: &SyncPipes,
    base: &std::ffi::CStr,
    dest: &std::ffi::CStr,
    src: &std::ffi::CStr,
) {
    let base_bytes = base.to_bytes();
    let dest_bytes = dest.to_bytes();
    let mut full_buf = Vec::with_capacity(base_bytes.len() + dest_bytes.len() + 1);
    full_buf.extend_from_slice(base_bytes);
    full_buf.extend_from_slice(dest_bytes);
    full_buf.push(0);
    let full =
        std::ffi::CStr::from_bytes_with_nul(&full_buf).expect("socket bind dest has valid NUL");
    let full_bytes = full.to_bytes();
    // Create the mountpoint directory chain under `base` (which already exists):
    // mkdir at each '/' past the base prefix, then the final component.
    let mut index = base_bytes.len() + 1;
    while index <= full_bytes.len() {
        if index == full_bytes.len() || full_bytes[index] == b'/' {
            let mut component = full_bytes[..index].to_vec();
            component.push(0);
            let component = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(&component) };
            if mkdir_mountpoint_component_no_symlink(component).is_err() {
                unsafe { child_setup_fail(pipes, b'k') };
            }
        }
        index += 1;
    }
    if unsafe {
        libc::mount(
            src.as_ptr(),
            full.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'k') };
    }

    // Remount the (read-WRITE) socket bind nosuid+nodev for defense-in-depth: the
    // workload only needs to bind a unix socket here, never a setuid binary or a
    // device node. Mount flags on the initial `MS_BIND` are ignored by the
    // kernel, so a `MS_REMOUNT|MS_BIND` pass is required to set them. Best-effort:
    // a kernel that rejects the remount must not fail the launch, since the bind
    // itself (the load-bearing step) already succeeded.
    let _ = unsafe {
        libc::mount(
            std::ptr::null(),
            full.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    };
}

fn mkdir_mountpoint_component_no_symlink(component: &std::ffi::CStr) -> Result<(), i32> {
    if unsafe { libc::mkdir(component.as_ptr(), 0o755) } == 0 {
        return Ok(());
    }

    let errno = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    if errno != libc::EEXIST {
        return Err(errno);
    }

    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::lstat(component.as_ptr(), stat.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    let stat = unsafe { stat.assume_init() };
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Ok(()),
        libc::S_IFLNK => Err(libc::ELOOP),
        _ => Err(libc::ENOTDIR),
    }
}

/// True if `path` is an existing regular file (checked with `lstat`, so a
/// symlink reports false). Used to tolerate `EEXIST` on a file mountpoint only
/// when the pre-existing entry is a real regular file and not a symlink that
/// could redirect the subsequent bind mount.
fn mountpoint_is_regular_file(path: &std::ffi::CStr) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::lstat(path.as_ptr(), stat.as_mut_ptr()) } < 0 {
        return false;
    }
    let stat = unsafe { stat.assume_init() };
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
}

/// Build `<base>/dev` as a fresh tmpfs and bind the host's character device
/// nodes into it. Runs privileged in the initial user namespace (pre-pivot), so
/// the `MS_BIND` mounts succeed without `CAP_MKNOD` (which the daemon lacks).
/// The nodes ride into the workload via the recursive self-bind + `pivot_root`
/// in `child_exec`. See ADR-026.
unsafe fn child_setup_dev(pipes: &SyncPipes, base: &std::ffi::CStr) {
    fn join_nul(base: &[u8], suffix: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(base.len() + suffix.len() + 1);
        v.extend_from_slice(base);
        v.extend_from_slice(suffix);
        v.push(0);
        v
    }
    let base_bytes = base.to_bytes();
    let dev_buf = join_nul(base_bytes, b"/dev");
    let dev_dir = std::ffi::CStr::from_bytes_with_nul(&dev_buf).expect("dev dir has valid NUL");

    // Minimal images omit /dev; create it, then mount a fresh tmpfs over it.
    if unsafe { libc::mkdir(dev_dir.as_ptr(), 0o755) } < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            unsafe { child_setup_fail(pipes, b'D') };
        }
    }
    if unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            dev_dir.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            c"mode=755,size=65536".as_ptr().cast(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'D') };
    }

    // Bind real host device nodes onto empty file mountpoints in the tmpfs.
    // null/zero/full/random/urandom are required; tty is best-effort.
    let nodes: &[(&std::ffi::CStr, &[u8], bool)] = &[
        (c"/dev/null", b"/dev/null", true),
        (c"/dev/zero", b"/dev/zero", true),
        (c"/dev/full", b"/dev/full", true),
        (c"/dev/random", b"/dev/random", true),
        (c"/dev/urandom", b"/dev/urandom", true),
        (c"/dev/tty", b"/dev/tty", false),
    ];
    for &(host_src, suffix, required) in nodes {
        let dst_buf = join_nul(base_bytes, suffix);
        let dst =
            std::ffi::CStr::from_bytes_with_nul(&dst_buf).expect("dev node path has valid NUL");
        let fd = unsafe {
            libc::open(
                dst.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC,
                0o666,
            )
        };
        if fd >= 0 {
            let _ = unsafe { libc::close(fd) };
        }
        let bound = unsafe {
            libc::mount(
                host_src.as_ptr(),
                dst.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        } >= 0;
        if !bound && required {
            unsafe { child_setup_fail(pipes, b'D') };
        }
    }

    // /dev/pts and /dev/shm directories (best-effort).
    for suffix in [b"/dev/pts".as_slice(), b"/dev/shm".as_slice()] {
        let buf = join_nul(base_bytes, suffix);
        let dir = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(&buf) };
        let _ = unsafe { libc::mkdir(dir.as_ptr(), 0o755) };
    }
}

unsafe fn child_exec(
    plan: &NativeLaunchPlan,
    pipes: &SyncPipes,
    argv: &[*const libc::c_char],
    env: &[*const libc::c_char],
    stdio: &ChildStdio,
) -> ! {
    if plan.setup.clone_flags & libc::CLONE_NEWNS != 0
        && let Err(e) = rustix::mount::mount_change(
            c"/",
            rustix::mount::MountPropagationFlags::PRIVATE
                | rustix::mount::MountPropagationFlags::REC,
        )
    {
        unsafe { child_setup_fail_errno(pipes, b'P', e.raw_os_error()) };
    }

    if let Some(stdout_fd) = stdio.stdout_fd {
        redirect_stdio_fd(pipes, stdout_fd, libc::STDOUT_FILENO);
    }
    if let Some(stderr_fd) = stdio.stderr_fd {
        redirect_stdio_fd(pipes, stderr_fd, libc::STDERR_FILENO);
    }

    // The new root was already mounted — privileged, in the initial user
    // namespace — by `child_prepare_root` before the user-namespace unshare.
    // Here we only select its path to pivot into. See ADR-026.
    let new_root = if let Some(overlay) = &plan.overlay {
        overlay.merged.as_c_str()
    } else {
        plan.rootfs.as_c_str()
    };

    // Ensure the manifest workdir exists in the new root before pivot_root; the
    // image may omit it and the post-pivot chdir would then ENOENT. Default "/"
    // always exists.
    let workdir_bytes = plan.workdir.to_bytes();
    if workdir_bytes != b"/" {
        let mut wd_buf = new_root.to_bytes().to_vec();
        if !wd_buf.ends_with(b"/") {
            wd_buf.push(b'/');
        }
        wd_buf.extend_from_slice(workdir_bytes.strip_prefix(b"/").unwrap_or(workdir_bytes));
        let wd_path = std::path::Path::new(std::ffi::OsStr::from_bytes(&wd_buf));
        if let Err(e) = std::fs::create_dir_all(wd_path) {
            unsafe { child_setup_fail_errno(pipes, b'w', e.raw_os_error().unwrap_or(libc::EIO)) };
        }
    }

    // The new root is either the overlay mounted before the user-namespace
    // unshare (now an inherited MNT_LOCKED mount that `pivot_root` rejects with
    // EINVAL) or the plain artifact rootfs. Bind it onto itself here, inside the
    // new user namespace, to get a fresh unlocked mount point to pivot into.
    if unsafe {
        libc::mount(
            new_root.as_ptr(),
            new_root.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'r') }; // new-root self-bind mount
    }

    let mut old_root_buf = new_root.to_bytes().to_vec();
    old_root_buf.extend_from_slice(b"/.old_root\0");
    let old_root_target =
        std::ffi::CStr::from_bytes_with_nul(&old_root_buf).expect("old_root path has valid NUL");

    if std::fs::create_dir(old_root_target.to_string_lossy().as_ref()).is_err() {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            unsafe { child_setup_fail(pipes, b'o') }; // create /.old_root
        }
    }

    if unsafe {
        libc::syscall(
            libc::SYS_pivot_root,
            new_root.as_ptr(),
            old_root_target.as_ptr(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'v') }; // pivot_root
    }

    if let Err(e) = rustix::process::chdir(c"/") {
        unsafe { child_setup_fail_errno(pipes, b'W', e.raw_os_error()) };
    }

    // Mount /proc and apply the masks BEFORE detaching /.old_root. A fresh
    // procfs mount in the workload's user namespace is denied EPERM unless a
    // fully-visible proc already exists as a reference (the kernel's
    // `mount_too_revealing` anti-spoofing rule for proc in a userns). The old
    // root's /proc — still mounted at /.old_root/proc until the detach below —
    // is that reference; detaching first makes the proc mount fail. (Verified:
    // proc-after-detach => EPERM, proc-before-detach => OK.) /dev and the
    // read-only binds were already set up pre-userns in `child_prepare_root`.
    if plan.setup.mount_proc {
        // Minimal images omit /proc; ensure the mount target exists.
        let _ = std::fs::create_dir_all("/proc");
        match mount_proc(&plan.proc_fs_type, &plan.proc_mount_target) {
            Ok(()) => {}
            // A proc already mounted at /proc is benign (e.g. inherited) — keep it.
            Err(e) if e == rustix::io::Errno::EXIST || e == rustix::io::Errno::BUSY => {}
            Err(e) => unsafe { child_setup_fail_errno(pipes, b'p', e.raw_os_error()) },
        }
    }

    if plan.mask_proc && mask_proc_paths().is_err() {
        unsafe { child_setup_fail(pipes, b'm') };
    }

    // The old root's /proc is no longer needed as the mount reference — detach
    // the whole old-root subtree now. /dev was set up pre-userns and read-only
    // binds travelled in via the recursive self-bind, so nothing else is needed
    // from /.old_root.
    if let Err(e) = rustix::mount::unmount(c"/.old_root", rustix::mount::UnmountFlags::DETACH) {
        unsafe { child_setup_fail_errno(pipes, b'u', e.raw_os_error()) }; // unmount old_root
    }
    let _ = unsafe { libc::rmdir(c"/.old_root".as_ptr()) };

    if unsafe { libc::chdir(plan.workdir.as_ptr()) } < 0 {
        unsafe { child_setup_fail(pipes, b'W') };
    }

    // Bring loopback up in the workload's own network namespace while we still
    // hold CAP_NET_ADMIN — execve strips the workload's capabilities (it is not
    // uid 0 inside the userns). A fresh netns has `lo` DOWN, and the socket-proxy
    // and workload talk over 127.0.0.1, which requires it UP.
    if plan.setup.clone_flags & libc::CLONE_NEWNET != 0
        && let Err(e) = unsafe { bring_loopback_up() }
    {
        unsafe { child_setup_fail_errno(pipes, b'n', e) };
    }

    if plan.setup.no_new_privs && !caps::try_set_no_new_privs() {
        unsafe { child_setup_fail(pipes, b'N') };
    }
    if plan.setup.drop_bounding_caps && !caps::try_drop_bounding_caps() {
        unsafe { child_setup_fail(pipes, b'B') };
    }

    if let Err(e) = apply_rlimits(plan.max_fds, plan.max_procs, plan.max_pids) {
        unsafe { child_setup_fail_errno(pipes, b'L', e.raw_os_error()) };
    }

    if plan.seccomp && seccomp::install_filter().is_err() {
        unsafe { child_setup_fail(pipes, b'S') };
    }

    // Defensively close any inherited daemon fds (SQLite, the Pingora :80/:443
    // listeners, log files, ACME state) before execve so a single non-CLOEXEC fd
    // anywhere in the daemon cannot leak into the workload across the pivot. std
    // and tokio open fds O_CLOEXEC by default, so today nothing leaks; this is a
    // belt-and-braces sweep matching runc/crun. stdin/stdout/stderr (0/1/2) were
    // already set up; the child's error pipe (`child_error_write`) is kept open
    // until execve so a late failure can still be reported, and it is O_CLOEXEC
    // so it closes automatically on a successful execve. See L1.
    unsafe { close_inherited_fds(pipes.child_error_write) };

    // Drop into the mapped user-namespace root (uid/gid 0 inside the userns =
    // host userns_base) as the LAST step before execve. The id-map is written by
    // now, and the child holds CAP_SETUID/SETGID in the userns (creator lineage),
    // so this succeeds. It MUST come after every privileged step above —
    // setresuid clears capabilities when leaving kernel-uid 0; execve as
    // userns-root then re-grants userns caps (unless no_new_privs was set for a
    // hardened launch). Without this the workload runs as the unmapped host uid
    // ("nobody" in the userns): capless and not the owner of the per-replica
    // layers + socket dir chowned to userns_base, so socket-proxy's bind() and
    // any upper-layer write fail EACCES. gid before uid. See ADR-026.
    if plan.setup.clone_flags & libc::CLONE_NEWUSER != 0 {
        if unsafe { libc::setresgid(0, 0, 0) } < 0 {
            unsafe { child_setup_fail(pipes, b'g') };
        }
        if unsafe { libc::setresuid(0, 0, 0) } < 0 {
            unsafe { child_setup_fail(pipes, b'i') };
        }
    }

    unsafe { libc::execve(plan.program.as_ptr(), argv.as_ptr(), env.as_ptr()) };
    unsafe { child_setup_fail(pipes, b'E') };
}

fn mount_proc(fs_type: &CString, target: &CString) -> Result<(), rustix::io::Errno> {
    rustix::mount::mount(
        fs_type,
        target,
        fs_type,
        rustix::mount::MountFlags::NOSUID
            | rustix::mount::MountFlags::NOEXEC
            | rustix::mount::MountFlags::NODEV,
        Option::<&std::ffi::CStr>::None,
    )
}

fn mask_proc_paths() -> Result<(), rustix::io::Errno> {
    // Directory entries that expose kernel state: bind read-only so they stay
    // readable (apps reading sysctls keep working) but immutable. File entries:
    // shadow with the real /dev/null bound in child_setup_dev. Best-effort — a
    // target absent on a minimal kernel, or an unsupported mask, is skipped
    // rather than failing the workload.
    let dir_targets: &[&std::ffi::CStr] = &[c"/proc/sys", c"/proc/irq", c"/proc/bus", c"/proc/fs"];
    let file_targets: &[&std::ffi::CStr] = &[
        c"/proc/sysrq-trigger",
        c"/proc/kcore",
        c"/proc/latency_stats",
        c"/proc/timer_list",
        c"/proc/sched_debug",
    ];
    for &target in dir_targets {
        let bound = unsafe {
            libc::mount(
                target.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
        } == 0;
        if bound {
            let _ = unsafe {
                libc::mount(
                    std::ptr::null(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                )
            };
        }
    }
    for &target in file_targets {
        let _ = unsafe {
            libc::mount(
                c"/dev/null".as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
    }
    Ok(())
}

fn apply_rlimits(
    max_fds: Option<u64>,
    max_procs: Option<u64>,
    max_pids: Option<u64>,
) -> Result<(), rustix::io::Errno> {
    if let Some(max) = max_fds {
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(max),
                maximum: Some(max),
            },
        )?;
    }
    // `RLIMIT_NPROC` is the per-uid process/thread cap. Both `max_procs` and
    // `max_pids` express that limit (the latter from the deployment/job
    // `pids_max`); apply the tighter of the two as a process-level backstop
    // complementing the cgroup `pids.max`. Without this, `max_pids` would be a
    // dead field and pid limiting would rely solely on the cgroup.
    let nproc = match (max_procs, max_pids) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let Some(max) = nproc {
        rustix::process::setrlimit(
            rustix::process::Resource::Nproc,
            rustix::process::Rlimit {
                current: Some(max),
                maximum: Some(max),
            },
        )?;
    }
    let _ = rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
    Ok(())
}

/// Apply a single read-only bind mount under `base`, the new-root path, while
/// the child is still privileged in the initial user namespace (pre-pivot,
/// pre-userns).
///
/// The full mountpoint is `base` + `dest`; `dest` is absolute within the new
/// root. Creates the parent directory chain under `base` and the mountpoint
/// itself (an empty file when `dest_is_file`, otherwise a directory),
/// bind-mounts the host-absolute `src` onto it, then remounts read-only. On any
/// failure the child reports stage `b'b'`.
unsafe fn child_apply_ro_bind(pipes: &SyncPipes, base: &std::ffi::CStr, bind: &RoBindPlan) {
    // Full destination = base (new-root path) + dest (absolute within new root).
    let base_bytes = base.to_bytes();
    let dest_bytes = bind.dest.to_bytes();
    let mut full_buf = Vec::with_capacity(base_bytes.len() + dest_bytes.len() + 1);
    full_buf.extend_from_slice(base_bytes);
    full_buf.extend_from_slice(dest_bytes);
    full_buf.push(0);
    let full_dest =
        std::ffi::CStr::from_bytes_with_nul(&full_buf).expect("ro bind dest path has valid NUL");
    let full_bytes = full_dest.to_bytes();

    // Create each parent directory component of the full dest. `base` already
    // exists; `dest` is absolute, so start past the base prefix and its leading
    // '/', and mkdir at every subsequent slash. The `base` tree is the overlay
    // `merged` view (image-controlled), so an existing component must be checked
    // with `lstat` and rejected if it is a symlink — otherwise a rootfs that
    // pre-creates `/.denia` (or `/.denia/lib`) as a symlink to an absolute host
    // path could redirect this privileged, pre-userns mountpoint creation (and
    // the subsequent bind) onto a host location outside the new root. This is the
    // same guard `child_bind_dir_rw` already applies to the socket bind dest;
    // both bind paths now share `mkdir_mountpoint_component_no_symlink`. See H1 /
    // ADR-026.
    let mut index = base_bytes.len() + 1;
    while index < full_bytes.len() {
        if full_bytes[index] == b'/' {
            let mut component = full_bytes[..index].to_vec();
            component.push(0);
            let component = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(&component) };
            if mkdir_mountpoint_component_no_symlink(component).is_err() {
                unsafe { child_setup_fail(pipes, b'b') };
            }
        }
        index += 1;
    }

    if bind.dest_is_file {
        // O_NOFOLLOW so a symlink planted at the final component is rejected
        // (ELOOP) instead of being followed onto a host path; O_EXCL so the file
        // mountpoint is freshly created. An EEXIST is tolerated only after an
        // `lstat` confirms the existing entry is a regular file (not a symlink or
        // other type), mirroring the no-symlink directory guard above.
        let fd = unsafe {
            libc::open(
                full_dest.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            let errno = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO);
            if errno != libc::EEXIST || !mountpoint_is_regular_file(full_dest) {
                unsafe { child_setup_fail(pipes, b'b') };
            }
        } else {
            let _ = unsafe { libc::close(fd) };
        }
    } else if mkdir_mountpoint_component_no_symlink(full_dest).is_err() {
        unsafe { child_setup_fail(pipes, b'b') };
    }

    // The bind source is a host-absolute path resolved in the initial mount
    // namespace (pre-pivot, pre-userns), so it is used directly with no
    // `/.old_root` prefix.
    if unsafe {
        libc::mount(
            bind.src.as_ptr(),
            full_dest.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'b') };
    }

    if unsafe {
        libc::mount(
            std::ptr::null(),
            full_dest.as_ptr(),
            std::ptr::null(),
            // RO + nosuid + nodev: a read-only helper bind must never expose a
            // setuid bit or a device node for defense-in-depth.
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    } < 0
    {
        unsafe { child_setup_fail(pipes, b'b') };
    }
}

struct ChildStdio {
    stdout_fd: Option<RawFd>,
    stderr_fd: Option<RawFd>,
}

fn open_child_stdio(plan: &NativeLaunchPlan, pipes: &SyncPipes) -> ChildStdio {
    ChildStdio {
        stdout_fd: plan
            .stdout_path
            .as_deref()
            .map(|path| open_stdio_file(pipes, path)),
        stderr_fd: plan
            .stderr_path
            .as_deref()
            .map(|path| open_stdio_file(pipes, path)),
    }
}

fn open_stdio_file(pipes: &SyncPipes, path: &std::ffi::CStr) -> RawFd {
    use rustix::fd::IntoRawFd;
    match rustix::fs::open(
        path,
        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::APPEND,
        rustix::fs::Mode::from_bits(0o644).expect("valid mode"),
    ) {
        Ok(fd) => fd.into_raw_fd(),
        Err(_) => unsafe { child_setup_fail(pipes, b'O') },
    }
}

fn redirect_stdio_fd(pipes: &SyncPipes, fd: RawFd, target_fd: libc::c_int) {
    if unsafe { libc::dup2(fd, target_fd) } < 0 {
        close_fd(fd);
        unsafe { child_setup_fail(pipes, b'O') };
    }
    close_fd(fd);
}

/// Close every inherited file descriptor at or above fd 3 except `keep`, just
/// before `execve`, so no stray daemon fd leaks into the workload across the
/// pivot. Uses the `close_range(2)` syscall directly (async-signal-safe: a
/// single raw syscall, no allocation). Closes `[3, keep-1]` and `[keep+1, ~0]`
/// so the still-needed error-reporting pipe survives. Best-effort: on a kernel
/// without `close_range` (< 5.9) the syscall returns `ENOSYS` and the existing
/// CLOEXEC discipline remains the guarantee, so the error is ignored.
unsafe fn close_inherited_fds(keep: RawFd) {
    const FIRST: libc::c_uint = 3;
    let keep = keep as libc::c_uint;
    if keep > FIRST {
        let _ = unsafe { libc::syscall(libc::SYS_close_range, FIRST, keep - 1, 0) };
    }
    let lo = std::cmp::max(FIRST, keep.saturating_add(1));
    let _ = unsafe { libc::syscall(libc::SYS_close_range, lo, libc::c_uint::MAX, 0) };
}

const CHILD_SETUP_EXIT_CODE: i32 = 127;
const CHILD_SETUP_TIMEOUT: Duration = Duration::from_secs(5);

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
    fn namespace_config_rejects_traversing_workdirs_before_fork() {
        for workdir in ["/../sock/pwn", "/srv/../sock", "/./srv"] {
            let err = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
                .with_uid_map(100000, 65536)
                .with_cgroup_path("/sys/fs/cgroup/denia/test")
                .with_workdir(workdir)
                .native_launch_plan()
                .expect_err("traversing workdir must be rejected before fork");
            assert!(
                err.to_string().contains("workdir"),
                "unexpected error for {workdir}: {err:?}"
            );
        }
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
    fn with_overlay_and_ro_bind_populate_config() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_overlay(OverlaySpec {
                lower: PathBuf::from("/var/lib/denia/artifacts/sha256-abc/rootfs"),
                upper: PathBuf::from("/var/lib/denia/replicas/r1/upper"),
                work: PathBuf::from("/var/lib/denia/replicas/r1/work"),
                merged: PathBuf::from("/var/lib/denia/replicas/r1/merged"),
            })
            .with_ro_bind(RoBind {
                src: PathBuf::from("/usr/lib/denia/socket-proxy"),
                dest: PathBuf::from("/.denia/socket-proxy"),
            })
            .with_ro_bind(RoBind {
                src: PathBuf::from("/usr/lib/denia/other"),
                dest: PathBuf::from("/.denia/other"),
            });

        assert_eq!(
            cfg.overlay,
            Some(OverlaySpec {
                lower: PathBuf::from("/var/lib/denia/artifacts/sha256-abc/rootfs"),
                upper: PathBuf::from("/var/lib/denia/replicas/r1/upper"),
                work: PathBuf::from("/var/lib/denia/replicas/r1/work"),
                merged: PathBuf::from("/var/lib/denia/replicas/r1/merged"),
            })
        );
        assert_eq!(
            cfg.ro_binds,
            vec![
                RoBind {
                    src: PathBuf::from("/usr/lib/denia/socket-proxy"),
                    dest: PathBuf::from("/.denia/socket-proxy"),
                },
                RoBind {
                    src: PathBuf::from("/usr/lib/denia/other"),
                    dest: PathBuf::from("/.denia/other"),
                },
            ]
        );
    }

    #[test]
    fn default_config_has_no_overlay_or_ro_binds() {
        let cfg = NamespaceConfig::default();
        assert_eq!(cfg.overlay, None);
        assert!(cfg.ro_binds.is_empty());
    }

    #[test]
    fn native_launch_plan_omits_overlay_and_ro_binds_by_default() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test");

        let plan = cfg.native_launch_plan().expect("native launch plan");

        assert_eq!(plan.overlay, None);
        assert!(plan.ro_binds.is_empty());
    }

    #[test]
    fn native_launch_plan_materializes_overlay_data_and_ro_binds() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_overlay(OverlaySpec {
                lower: PathBuf::from("/lower"),
                upper: PathBuf::from("/upper"),
                work: PathBuf::from("/work"),
                merged: PathBuf::from("/merged"),
            })
            .with_ro_bind(RoBind {
                src: PathBuf::from("/usr/lib/denia/socket-proxy"),
                dest: PathBuf::from("/.denia/socket-proxy"),
            });

        let plan = cfg.native_launch_plan().expect("native launch plan");

        let overlay = plan.overlay.expect("overlay plan");
        assert_eq!(overlay.merged.to_str().expect("merged"), "/merged");
        assert_eq!(
            overlay.overlay_fs_type.to_str().expect("fs type"),
            "overlay"
        );
        assert_eq!(
            overlay.data.to_str().expect("data"),
            "lowerdir=/lower,upperdir=/upper,workdir=/work"
        );

        assert_eq!(plan.ro_binds.len(), 1);
        let bind = &plan.ro_binds[0];
        assert_eq!(
            bind.src.to_str().expect("src"),
            "/usr/lib/denia/socket-proxy"
        );
        assert_eq!(bind.dest.to_str().expect("dest"), "/.denia/socket-proxy");
        // The host source does not exist in the test environment, so it is not
        // a regular file and the mountpoint is created as a directory.
        assert!(!bind.dest_is_file);
    }

    #[test]
    fn native_launch_plan_marks_existing_file_ro_bind_as_file() {
        let src = tempfile::NamedTempFile::new().expect("temp file");
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_ro_bind(RoBind {
                src: src.path().to_path_buf(),
                dest: PathBuf::from("/.denia/socket-proxy"),
            });

        let plan = cfg.native_launch_plan().expect("native launch plan");

        assert!(plan.ro_binds[0].dest_is_file);
    }

    #[test]
    fn native_launch_plan_rejects_relative_ro_bind_dest() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_ro_bind(RoBind {
                src: PathBuf::from("/usr/lib/denia/socket-proxy"),
                dest: PathBuf::from("relative/socket-proxy"),
            });

        let error = cfg
            .native_launch_plan()
            .expect_err("relative ro bind dest must be rejected");

        assert!(
            matches!(error, SyscallError::Capability(ref reason) if reason.contains("ro bind dest must be absolute")),
            "expected ro bind dest error, got: {error:?}"
        );
    }

    #[test]
    fn socket_bind_mountpoint_creation_rejects_symlink_components() {
        let root = tempfile::tempdir().expect("root");
        let escaped = tempfile::tempdir().expect("escaped");
        let link = root.path().join("var");
        std::os::unix::fs::symlink(escaped.path(), &link).expect("symlink");
        let component = CString::new(link.as_os_str().as_bytes()).expect("component");

        let err = mkdir_mountpoint_component_no_symlink(&component)
            .expect_err("symlink components must be rejected");

        assert_eq!(err, libc::ELOOP);
        assert!(!escaped.path().join("lib").exists());
    }

    #[test]
    fn ro_bind_mountpoint_creation_rejects_symlink_components() {
        // The RO-bind mountpoint chain (child_apply_ro_bind) shares the same
        // symlink-rejecting component creator as the socket bind, so an
        // image-controlled `merged` base that pre-creates `/.denia` as a symlink
        // to an absolute host path cannot redirect the privileged, pre-userns
        // mountpoint creation outside the new root. See H1 / ADR-026.
        let root = tempfile::tempdir().expect("root");
        let escaped = tempfile::tempdir().expect("escaped");
        let link = root.path().join(".denia");
        std::os::unix::fs::symlink(escaped.path(), &link).expect("symlink");
        let component = CString::new(link.as_os_str().as_bytes()).expect("component");

        let err = mkdir_mountpoint_component_no_symlink(&component)
            .expect_err("symlink mountpoint components must be rejected");

        assert_eq!(err, libc::ELOOP);
        assert!(!escaped.path().join("socket-proxy").exists());
    }

    #[test]
    fn mountpoint_is_regular_file_rejects_symlink_and_dir() {
        let dir = tempfile::tempdir().expect("dir");
        let file_path = dir.path().join("real");
        std::fs::write(&file_path, b"x").expect("write file");
        let file_c = CString::new(file_path.as_os_str().as_bytes()).expect("file cstr");
        assert!(mountpoint_is_regular_file(&file_c));

        let link_path = dir.path().join("link");
        std::os::unix::fs::symlink(&file_path, &link_path).expect("symlink");
        let link_c = CString::new(link_path.as_os_str().as_bytes()).expect("link cstr");
        assert!(
            !mountpoint_is_regular_file(&link_c),
            "a symlink (even to a regular file) must not be accepted as a file mountpoint"
        );

        let dir_c = CString::new(dir.path().as_os_str().as_bytes()).expect("dir cstr");
        assert!(!mountpoint_is_regular_file(&dir_c));

        let missing_c = CString::new(dir.path().join("missing").as_os_str().as_bytes())
            .expect("missing cstr");
        assert!(!mountpoint_is_regular_file(&missing_c));
    }

    #[test]
    fn native_launch_plan_rejects_overlay_separator_in_paths() {
        let cfg = NamespaceConfig::new("/var/lib/denia/rootfs", vec!["/bin/sh".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_overlay(OverlaySpec {
                lower: PathBuf::from("/low,er"),
                upper: PathBuf::from("/upper"),
                work: PathBuf::from("/work"),
                merged: PathBuf::from("/merged"),
            });

        let error = cfg
            .native_launch_plan()
            .expect_err("overlay separator must be rejected");

        assert!(
            matches!(error, SyscallError::Capability(ref reason) if reason.contains("overlay option separators")),
            "expected overlay separator error, got: {error:?}"
        );
    }

    #[test]
    fn child_setup_stage_names_ro_bind_failure() {
        assert_eq!(child_setup_stage(b'b'), "read-only bind mount");
    }

    #[test]
    fn null_terminated_ptrs_prepares_execve_payload_before_fork() {
        let argv = vec![
            CString::new("/bin/sh").expect("program"),
            CString::new("-c").expect("arg"),
        ];

        let ptrs = null_terminated_ptrs(&argv);

        assert_eq!(ptrs.len(), 3);
        assert_eq!(ptrs[0], argv[0].as_ptr());
        assert_eq!(ptrs[1], argv[1].as_ptr());
        assert!(ptrs[2].is_null());
    }

    #[test]
    fn child_setup_stage_names_common_failures() {
        assert_eq!(child_setup_stage(b'U'), "unshare mount namespace");
        assert_eq!(child_setup_stage(b'X'), "unshare user/pid namespace");
        assert_eq!(child_setup_stage(b'a'), "chroot: overlay mount");
        assert_eq!(child_setup_stage(b'R'), "chroot");
        assert_eq!(child_setup_stage(b'E'), "execve");
        assert_eq!(child_setup_stage(b'?'), "unknown setup stage");
    }

    #[test]
    fn read_optional_byte_timeout_reports_timeout_without_eof() {
        let pipe = pipe_cloexec().expect("pipe");

        let result = read_optional_byte_timeout(pipe[0], Duration::ZERO);

        assert!(matches!(result, TimedByte::Timeout));
        close_fd(pipe[0]);
        close_fd(pipe[1]);
    }

    #[test]
    fn read_optional_byte_timeout_reads_written_byte() {
        let pipe = pipe_cloexec().expect("pipe");
        write_byte(pipe[1], b'X').expect("write byte");

        let result = read_optional_byte_timeout(pipe[0], Duration::from_secs(1));

        assert!(matches!(result, TimedByte::Byte(b'X')));
        close_fd(pipe[0]);
        close_fd(pipe[1]);
    }

    #[test]
    fn deferred_hardening_leaves_stage_one_capabilities_available() {
        let cfg = NamespaceConfig::new("/tmp/rootfs", vec!["/bin/true".to_string()])
            .with_uid_map(100000, 65536)
            .with_cgroup_path("/sys/fs/cgroup/denia/test")
            .with_deferred_hardening();

        let plan = cfg.setup_plan().expect("setup plan");

        assert!(!plan.no_new_privs);
        assert!(!plan.drop_bounding_caps);
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
