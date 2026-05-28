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
        let overlay = self.overlay.as_ref().map(overlay_plan).transpose()?;
        let ro_binds = self
            .ro_binds
            .iter()
            .map(ro_bind_plan)
            .collect::<Result<Vec<_>, _>>()?;

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
    // `userxattr` lets the overlay use the `user.overlay.*` xattr namespace
    // instead of `trusted.overlay.*`, which is the only namespace
    // unprivileged user-namespace mounters can set. Required on Linux 5.11+
    // when the mount happens inside an unprivileged userns (CAP_SYS_ADMIN in
    // the userns but not in the initial namespace). Without it the mount
    // syscall fails with EACCES.
    data.extend_from_slice(b",userxattr");
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
    let src_cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let parent_cg = std::fs::read_to_string("/proc/self/cgroup")
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let leaf_dir = cgroup_procs_path.parent().unwrap_or(cgroup_procs_path);
    let leaf_type = std::fs::read_to_string(leaf_dir.join("cgroup.type"))
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let leaf_controllers = std::fs::read_to_string(leaf_dir.join("cgroup.controllers"))
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let leaf_subtree = std::fs::read_to_string(leaf_dir.join("cgroup.subtree_control"))
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let leaf_events = std::fs::read_to_string(leaf_dir.join("cgroup.events"))
        .unwrap_or_else(|e| format!("<read err: {e}>"));
    let pid_status_state = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .map(|s| {
            s.lines()
                .filter(|l| {
                    l.starts_with("State:")
                        || l.starts_with("Tgid:")
                        || l.starts_with("Pid:")
                        || l.starts_with("PPid:")
                        || l.starts_with("Threads:")
                })
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    tracing::info!(
        pid,
        target = %cgroup_procs_path.display(),
        src_cgroup = %src_cg.trim(),
        parent_cgroup = %parent_cg.trim(),
        leaf_type = %leaf_type.trim(),
        leaf_controllers = %leaf_controllers.trim(),
        leaf_subtree = %leaf_subtree.trim(),
        leaf_events = %leaf_events.trim().replace('\n', " | "),
        pid_state = %pid_status_state.trim(),
        "cgroup attach attempt"
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
        TimedByte::Timeout => unreachable!("read_byte_now never returns timeout"),
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
        b'U' => "unshare",
        b'M' => "id-map release",
        b'F' => "pid namespace fork",
        b'P' => "make mount propagation private",
        b'O' => "open stdio",
        b'R' => "chroot",
        b'a' => "chroot: overlay mount",
        b'r' => "chroot: rootfs self-bind mount",
        b'o' => "chroot: create /.old_root",
        b'v' => "chroot: pivot_root",
        b'u' => "chroot: unmount /.old_root",
        b'b' => "read-only bind mount",
        b'W' => "chdir workdir",
        b'p' => "mount proc",
        b'D' => "setup /dev tmpfs",
        b'm' => "mask /proc paths",
        b'N' => "set no_new_privs",
        b'B' => "drop capability bounding set",
        b'L' => "apply resource limits",
        b'S' => "install seccomp filter",
        b'E' => "execve",
        _ => "unknown setup stage",
    }
}

unsafe fn child_setup_fail(pipes: &SyncPipes, stage: u8) -> ! {
    // Capture errno BEFORE any further syscall (write_byte uses libc::write).
    let errno = unsafe { *libc::__errno_location() };
    // Pre-format a single 5-byte payload [stage, e0, e1, e2, e3] so the
    // parent can read errno alongside the stage tag without changing the
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

    if unsafe { libc::unshare(plan.setup.clone_flags) } < 0 {
        unsafe { child_setup_fail(pipes, b'U') };
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
    let pid = rustix::process::Pid::from_raw(pid).expect("pid from fork must be non-zero");
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

unsafe fn child_exec(
    plan: &NativeLaunchPlan,
    pipes: &SyncPipes,
    argv: &[*const libc::c_char],
    env: &[*const libc::c_char],
    stdio: &ChildStdio,
) -> ! {
    if plan.setup.clone_flags & libc::CLONE_NEWNS != 0
        && rustix::mount::mount_change(
            c"/",
            rustix::mount::MountPropagationFlags::PRIVATE
                | rustix::mount::MountPropagationFlags::REC,
        )
        .is_err()
    {
        unsafe { child_setup_fail(pipes, b'P') };
    }

    if let Some(stdout_fd) = stdio.stdout_fd {
        redirect_stdio_fd(pipes, stdout_fd, libc::STDOUT_FILENO);
    }
    if let Some(stderr_fd) = stdio.stderr_fd {
        redirect_stdio_fd(pipes, stderr_fd, libc::STDERR_FILENO);
    }

    // Select the path that becomes the new root. With an overlay, mount the
    // private overlay at `merged` and pivot into it; otherwise bind-mount the
    // shared `rootfs` onto itself (the existing behavior) so it is a mount
    // point that `pivot_root` accepts.
    let new_root = if let Some(overlay) = &plan.overlay {
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
        if unsafe {
            libc::mount(
                plan.rootfs.as_ptr(),
                plan.rootfs.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
        } < 0
        {
            unsafe { child_setup_fail(pipes, b'r') }; // rootfs self-bind mount
        }
        plan.rootfs.as_c_str()
    };

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

    if rustix::process::chdir(c"/").is_err() {
        unsafe { child_setup_fail(pipes, b'W') };
    }

    if rustix::mount::unmount(c"/.old_root", rustix::mount::UnmountFlags::DETACH).is_err() {
        unsafe { child_setup_fail(pipes, b'u') }; // unmount old_root
    }
    let _ = unsafe { libc::rmdir(c"/.old_root".as_ptr()) };

    // Inject read-only bind mounts now that the new root is in place and the
    // upper layer is writable. Each dest is absolute within the new root.
    for bind in &plan.ro_binds {
        unsafe { child_apply_ro_bind(pipes, bind) };
    }

    if unsafe { libc::chdir(plan.workdir.as_ptr()) } < 0 {
        unsafe { child_setup_fail(pipes, b'W') };
    }

    if plan.setup.mount_proc && mount_proc(&plan.proc_fs_type, &plan.proc_mount_target).is_err() {
        unsafe { child_setup_fail(pipes, b'p') };
    }

    if plan.setup_dev && setup_dev_tmpfs().is_err() {
        unsafe { child_setup_fail(pipes, b'D') };
    }

    if plan.mask_proc && mask_proc_paths().is_err() {
        unsafe { child_setup_fail(pipes, b'm') };
    }

    if plan.setup.no_new_privs && !caps::try_set_no_new_privs() {
        unsafe { child_setup_fail(pipes, b'N') };
    }
    if plan.setup.drop_bounding_caps && !caps::try_drop_bounding_caps() {
        unsafe { child_setup_fail(pipes, b'B') };
    }

    if apply_rlimits(plan.max_pids, plan.max_fds, plan.max_procs).is_err() {
        unsafe { child_setup_fail(pipes, b'L') };
    }

    if plan.seccomp && seccomp::install_filter().is_err() {
        unsafe { child_setup_fail(pipes, b'S') };
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

fn setup_dev_tmpfs() -> Result<(), rustix::io::Errno> {
    rustix::mount::mount(
        c"tmpfs",
        c"/dev",
        c"tmpfs",
        rustix::mount::MountFlags::NOSUID | rustix::mount::MountFlags::NOEXEC,
        Some(c"mode=755,size=65536"),
    )?;
    unsafe {
        create_dev_node(c"/dev/null", 1, 3, 0o666);
        create_dev_node(c"/dev/zero", 1, 5, 0o666);
        create_dev_node(c"/dev/full", 1, 7, 0o666);
        create_dev_node(c"/dev/random", 1, 8, 0o666);
        create_dev_node(c"/dev/urandom", 1, 9, 0o666);
        create_dev_node(c"/dev/tty", 5, 0, 0o666);
    }
    let _ = std::fs::create_dir_all("/dev/pts");
    let _ = std::fs::create_dir_all("/dev/shm");
    Ok(())
}

unsafe fn create_dev_node(path: &std::ffi::CStr, major: u32, minor: u32, mode: libc::mode_t) {
    unsafe {
        let dev = libc::makedev(major, minor);
        if libc::mknod(path.as_ptr(), libc::S_IFCHR | mode, dev) < 0 {
            let _ = libc::symlink(c"/dev/null".as_ptr(), path.as_ptr());
        }
    }
}

fn mask_proc_paths() -> Result<(), rustix::io::Errno> {
    let targets: &[&std::ffi::CStr] = &[
        c"/proc/sys",
        c"/proc/sysrq-trigger",
        c"/proc/irq",
        c"/proc/bus",
        c"/proc/fs",
        c"/proc/latency_stats",
        c"/proc/timer_list",
        c"/proc/sched_debug",
    ];
    for &target in targets {
        let _ = rustix::mount::mount_bind_recursive(c"/dev/null", target);
    }
    Ok(())
}

fn apply_rlimits(
    _max_pids: Option<u64>,
    max_fds: Option<u64>,
    max_procs: Option<u64>,
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
    if let Some(max) = max_procs {
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

/// Apply a single read-only bind mount inside the freshly pivoted root.
///
/// Creates `dest`'s parent directory chain and the mountpoint itself (an empty
/// file when `dest_is_file`, otherwise a directory), bind-mounts `src` onto it,
/// then remounts read-only. On any failure the child reports stage `b'b'`.
unsafe fn child_apply_ro_bind(pipes: &SyncPipes, bind: &RoBindPlan) {
    let dest_bytes = bind.dest.to_bytes();

    // Create each parent directory component of dest. `dest` is absolute, so
    // the first byte is '/'; skip it and mkdir at every subsequent slash.
    let mut index = 1;
    while index < dest_bytes.len() {
        if dest_bytes[index] == b'/' {
            let mut component = dest_bytes[..index].to_vec();
            component.push(0);
            let component = unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(&component) };
            if unsafe { libc::mkdir(component.as_ptr(), 0o755) } < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EEXIST) {
                    unsafe { child_setup_fail(pipes, b'b') };
                }
            }
        }
        index += 1;
    }

    if bind.dest_is_file {
        let fd = unsafe {
            libc::open(
                bind.dest.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                unsafe { child_setup_fail(pipes, b'b') };
            }
        } else {
            let _ = unsafe { libc::close(fd) };
        }
    } else if unsafe { libc::mkdir(bind.dest.as_ptr(), 0o755) } < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            unsafe { child_setup_fail(pipes, b'b') };
        }
    }

    if unsafe {
        libc::mount(
            bind.src.as_ptr(),
            bind.dest.as_ptr(),
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
            bind.dest.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID,
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
            "lowerdir=/lower,upperdir=/upper,workdir=/work,userxattr"
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
        assert_eq!(child_setup_stage(b'U'), "unshare");
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
