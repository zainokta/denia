use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use crate::artifacts::ArtifactKind;
use crate::domain::{
    JobOutcome, JobRunRequest, RuntimeInstanceId, RuntimeStartRequest, RuntimeStatus,
};
use crate::runtime::console::{RuntimeConsoleRequest, RuntimeConsoleSession};
use crate::runtime::error::RuntimeError;
use crate::runtime::fs_helpers::{
    cpu_max, create_dir_all, create_runtime_directory, exit_code_from_process_status, path_io,
    prepare_cgroup_directory, remove_cgroup_dir_if_exists, remove_dir_if_exists,
    remove_existing_runtime_file, resolve_host_binary, safe_artifact_name, terminate_tracked_child,
    validate_runtime_directory, wait_for_service_socket,
};
use crate::runtime::plan::{
    LinuxRuntimePlan, LinuxRuntimeProcessSpec, TrackedChild, TrackedProcess,
};
use crate::runtime::runtime_trait::Runtime;
use crate::runtime::validation::{
    validate_environment_keys, validate_process_spec, validate_resource_limits,
    validate_service_name,
};
use crate::syscall::SyscallError;
use crate::syscall::caps;
use crate::syscall::chown;
use crate::syscall::console::{
    ConsoleLaunchConfig, read_process_start_time, spawn_console_process,
};
use crate::syscall::ns::{NamespaceConfig, OverlaySpec, RoBind, spawn_namespaced_process};
use crate::syscall::pty::open_pty;
use crate::syscall::signal;

#[derive(Debug, Clone)]
pub struct LinuxRuntime {
    runtime_dir: PathBuf,
    artifact_dir: PathBuf,
    cgroup_root: PathBuf,
    log_dir: PathBuf,
    socket_proxy_source: PathBuf,
    userns_base: u32,
    userns_size: u32,
    children: Arc<Mutex<HashMap<RuntimeInstanceId, TrackedChild>>>,
    /// Backstop set of live console-shell pids. The console bridge reaps its
    /// child explicitly on session end (SIGTERM->grace->SIGKILL->waitpid), but
    /// if that task is dropped/panics the pid would otherwise leak as a zombie.
    /// `reap_exited_children` sweeps this set on every runtime mutation so a
    /// console child is always collected. See ADR-033 / review 07 (HIGH).
    console_children: Arc<Mutex<Vec<u32>>>,
}

pub(crate) const SOCKET_PROXY_TARGET: &str = "/.denia/socket-proxy";
/// Guest directory where socket-proxy's host shared libs + dynamic loader are
/// bound read-only, so socket-proxy runs regardless of the workload image's libc.
pub(crate) const SOCKET_PROXY_LIB_DIR: &str = "/.denia/lib";

/// Shared objects + dynamic loader the socket-proxy binary needs at runtime,
/// taken from the daemon's own `/proc/self/maps` (socket-proxy IS the daemon
/// binary). Each is bound read-only into the guest under `SOCKET_PROXY_LIB_DIR`
/// and socket-proxy is launched through the bound loader with `--library-path`,
/// so it works in any workload image. Empty for a fully static binary (then
/// socket-proxy is exec'd directly). See ADR-026.
fn socket_proxy_runtime_libs() -> Vec<PathBuf> {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut libs = Vec::new();
    for line in maps.lines() {
        // Fields: addr perms offset dev inode pathname
        let Some(path) = line.split_whitespace().nth(5) else {
            continue;
        };
        if path.starts_with('/') && path.contains(".so") && seen.insert(path.to_string()) {
            libs.push(PathBuf::from(path));
        }
    }
    libs
}

/// True if `path` is a dynamic loader (`ld-linux-*`, `ld-musl-*`).
fn is_dynamic_loader(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("ld-"))
}

/// Parse a directory entry whose name is a UUID (service/deployment dirs).
/// Returns `None` for non-dirs, symlinks, or non-UUID names so the orphan
/// sweep skips bookkeeping dirs like `jobs`.
fn parse_uuid_dir(path: &Path) -> Option<uuid::Uuid> {
    if !path.is_dir() {
        return None;
    }
    uuid::Uuid::parse_str(path.file_name()?.to_str()?).ok()
}

/// Parse a directory entry whose name is a numeric replica index.
fn parse_index_dir(path: &Path) -> Option<u32> {
    if !path.is_dir() {
        return None;
    }
    path.file_name()?.to_str()?.parse::<u32>().ok()
}
pub(crate) const WORKLOAD_LAUNCHER_TARGET: &str = "/.denia/workload-launcher";
pub(crate) const SOCKET_BASENAME: &str = "service.sock";
pub(crate) const GUEST_SERVICE_SOCKET_ENV: &str = "DENIA_SERVICE_SOCKET";
pub(crate) const CGROUP_CONTROLLERS: &[&str] = &["cpu", "memory", "pids", "io"];

impl LinuxRuntime {
    pub fn new(runtime_dir: impl Into<PathBuf>) -> Self {
        let runtime_dir = runtime_dir.into();
        let data_dir = runtime_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/var/lib/denia"));
        Self::new_with_paths(
            runtime_dir,
            data_dir.join("artifacts"),
            "/sys/fs/cgroup/denia",
        )
    }

    pub fn new_with_paths(
        runtime_dir: impl Into<PathBuf>,
        artifact_dir: impl Into<PathBuf>,
        cgroup_root: impl Into<PathBuf>,
    ) -> Self {
        let runtime_dir = runtime_dir.into();
        let log_dir = runtime_dir
            .parent()
            .map(|data_dir| data_dir.join("logs"))
            .unwrap_or_else(|| PathBuf::from("/var/lib/denia/logs"));
        Self {
            runtime_dir,
            artifact_dir: artifact_dir.into(),
            cgroup_root: cgroup_root.into(),
            log_dir,
            socket_proxy_source: std::env::current_exe().unwrap_or_else(|_| "denia".into()),
            userns_base: 100000,
            userns_size: 65536,
            children: Arc::new(Mutex::new(HashMap::new())),
            console_children: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_userns(mut self, base: u32, size: u32) -> Self {
        self.userns_base = base;
        self.userns_size = size;
        self
    }

    pub fn with_log_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_dir = path.into();
        self
    }

    pub fn with_socket_proxy(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_proxy_source = path.into();
        self
    }

    pub fn plan(&self, request: &RuntimeStartRequest) -> Result<LinuxRuntimePlan, RuntimeError> {
        validate_service_name(&request.service_name)?;
        if request.artifact.kind != ArtifactKind::RootfsBundle {
            return Err(RuntimeError::UnsupportedArtifactKind {
                kind: request.artifact.kind.clone(),
            });
        }

        let bundle_dir = self
            .artifact_dir
            .join(safe_artifact_name(&request.artifact.digest));
        let rootfs_path = bundle_dir.join("rootfs");
        if !rootfs_path.exists() {
            return Err(RuntimeError::MissingRootfs { path: rootfs_path });
        }
        validate_runtime_directory(&rootfs_path)?;
        let manifest_path = bundle_dir.join("process.json");
        let manifest = std::fs::read_to_string(&manifest_path)?;
        let process: LinuxRuntimeProcessSpec = serde_json::from_str(&manifest)?;
        validate_process_spec(&process, &manifest_path)?;
        let mut env_map: std::collections::BTreeMap<String, String> =
            process.env.into_iter().collect();
        for (key, value) in &request.env {
            env_map.insert(key.clone(), value.clone());
        }
        let merged_env_for_validation: Vec<(String, String)> = env_map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        validate_environment_keys(&merged_env_for_validation)?;
        let service_dir = self.runtime_dir.join(request.service_id.to_string());
        let deployment_dir = service_dir.join(request.deployment_id.to_string());
        let replica_dir = deployment_dir.join(request.replica_index.to_string());
        let upper = replica_dir.join("upper");
        let work = replica_dir.join("work");
        let merged = replica_dir.join("merged");
        // Per-replica socket dir on the REAL host fs (NOT the overlay): an
        // AF_UNIX socket created on overlayfs binds to the overlay inode and is
        // NOT connectable via the upperdir path. This dir is RW-bind-mounted onto
        // the SAME absolute path inside the guest (identity mount, see
        // with_socket_bind), so socket-proxy binds and the daemon dials the
        // identical sun_path string. That keeps Pingora's UDS connection-reuse
        // check (`getpeername` == dial path) passing — a divergent guest path
        // would log `unix FD mismatch` and force a fresh connection per request.
        // The hashed dir keeps the path under the sockaddr_un 108-byte limit.
        // `socket_path`/`socket_connect_path` are that host path. See ADR-026.
        let socket_dir_host = {
            // `DefaultHasher` is not collision-resistant, but the input space is a
            // single node's own (service:deployment:replica) ids — tiny and fully
            // operator-owned, not attacker-chosen — so a practical collision is
            // negligible. The hash only needs to keep the socket dir name short
            // (under the 108-byte sockaddr_un limit), not to be cryptographic (L4).
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            format!(
                "{}:{}:{}",
                request.service_id, request.deployment_id, request.replica_index
            )
            .hash(&mut hasher);
            self.runtime_dir
                .parent()
                .unwrap_or(self.runtime_dir.as_path())
                .join("sock")
                .join(format!("{:016x}", hasher.finish()))
        };
        let socket_path = socket_dir_host.join(SOCKET_BASENAME);
        let socket_connect_path = socket_path.clone();
        // Identity path: socket-proxy `--listen`, the workload's DENIA_SERVICE_SOCKET
        // env, and the daemon's dial target are all this single host path.
        let guest_socket = socket_path.to_string_lossy().into_owned();
        env_map.insert(GUEST_SERVICE_SOCKET_ENV.to_string(), guest_socket.clone());
        let env: Vec<(String, String)> = env_map.into_iter().collect();
        let cgroup_path = self
            .cgroup_root
            .join(request.service_id.to_string())
            .join(request.deployment_id.to_string())
            .join(request.replica_index.to_string());
        // socket-proxy is the daemon binary, dynamically linked against the host
        // libc/loader. It is bound read-only into the guest (with its host libs +
        // loader under SOCKET_PROXY_LIB_DIR) and run through the host loader with
        // `--library-path`, so it works in ANY workload image regardless of the
        // image's libc/version. The loader options are consumed at socket-proxy
        // startup and never reach the workload it spawns. See ADR-026.
        let (ro_binds, loader_prefix) = self.runtime_binary_binds(SOCKET_PROXY_TARGET)?;

        let mut child_argv = loader_prefix;
        child_argv.push(SOCKET_PROXY_TARGET.to_string());
        child_argv.push("--listen".to_string());
        child_argv.push(guest_socket.clone());
        child_argv.push("--connect".to_string());
        child_argv.push(format!("127.0.0.1:{}", request.internal_port));
        child_argv.push("--".to_string());
        child_argv.extend(process.argv);

        let overlay = OverlaySpec {
            lower: rootfs_path.clone(),
            upper: upper.clone(),
            work: work.clone(),
            merged: merged.clone(),
        };

        // The overlay's `merged` mountpoint becomes the new root (pivots into it
        // when an overlay is set), so the namespace root must be `merged`.
        let mut namespace = NamespaceConfig::new(merged.clone(), child_argv.clone())
            .with_overlay(overlay)
            .with_socket_bind(socket_dir_host.clone(), socket_dir_host.clone())
            .with_uid_map(self.userns_base, self.userns_size)
            .with_cgroup_path(cgroup_path.clone())
            .with_workdir(process.workdir.clone())
            .with_env(env.clone())
            .with_deferred_hardening();
        // Mirror the deployment pids cap onto RLIMIT_NPROC as a process-level
        // backstop to the cgroup `pids.max` (defaults to NamespaceConfig's value
        // when unset).
        if let Some(pids) = request.pids_max {
            namespace = namespace.with_max_pids(Some(pids));
        }
        for bind in ro_binds {
            namespace = namespace.with_ro_bind(bind);
        }

        Ok(LinuxRuntimePlan {
            namespace,
            rootfs_path,
            socket_path,
            socket_connect_path,
            guest_socket_path: guest_socket,
            cgroup_path,
            deployment_id: request.deployment_id,
            service_dir,
            deployment_dir,
            replica_dir,
            upper,
            work,
            merged,
            artifact_dir: self.artifact_dir.clone(),
            runtime_dir: self.runtime_dir.clone(),
        })
    }

    pub fn prepare(
        &self,
        plan: &LinuxRuntimePlan,
        request: &RuntimeStartRequest,
    ) -> Result<(), RuntimeError> {
        validate_resource_limits(request)?;
        // The artifact rootfs is the read-only overlay lower; never write into it.
        // The writable layer and overlay mountpoint are per-replica.

        // Unmount any stale overlay from previous failed deployments
        let _ = rustix::mount::unmount(&plan.merged, rustix::mount::UnmountFlags::DETACH);

        // Clean up overlay-specific state (the work/work directory that overlayfs creates)
        // This must be empty for a fresh overlay mount
        let overlay_work = plan.work.join("work");
        if overlay_work.exists() {
            std::fs::remove_dir_all(&overlay_work).map_err(path_io(
                "remove stale overlay work directory",
                &overlay_work,
            ))?;
        }

        create_dir_all("create replica upper directory", &plan.upper)?;
        create_dir_all("create replica work directory", &plan.work)?;
        create_dir_all("create replica merged directory", &plan.merged)?;

        // Persistent ancestor dirs: traverse-only (mode 0755), ownership left
        // with the daemon so it can keep creating future deployment/replica
        // subdirs. Only the ephemeral overlay layers below are chowned to the
        // userns base uid.
        self.set_traverse_mode(&plan.service_dir)?;
        self.set_traverse_mode(&plan.deployment_dir)?;
        self.set_traverse_mode(&plan.replica_dir)?;
        // Ephemeral, guest-writable overlay layers chowned to the namespace-root
        // (userns_base, = uid 0 inside the workload userns) so the workload owns
        // its writable upper layer. The overlay is mounted privileged in the
        // initial user namespace now (see syscall::ns / ADR-026), so this is no
        // longer required for the mount to succeed — it keeps guest writes and
        // copy-ups owned by the workload. `merged` is the mountpoint.
        self.chown_overlay_dir(&plan.upper)?;
        self.chown_overlay_dir(&plan.work)?;
        self.chown_overlay_dir(&plan.merged)?;
        // The child needs traverse permission (mode 0755 `other` x bit) to reach
        // the read-only lowerdir (artifact rootfs); it never writes there, so
        // ownership stays put and we avoid chowning thousands of artifact files.
        // The data dir (e.g. /var/lib/denia) is typically mode 0700, so it too
        // needs the traverse bit widened.
        if let Some(data_dir) = plan.artifact_dir.parent() {
            self.set_traverse_mode(data_dir)?;
        }
        self.set_traverse_mode(&plan.runtime_dir)?;
        self.set_traverse_mode(&plan.artifact_dir)?;
        if let Some(bundle_dir) = plan.rootfs_path.parent() {
            self.set_traverse_mode(bundle_dir)?;
            self.set_traverse_mode(&plan.rootfs_path)?;
        }
        self.prepare_socket_directory(plan)?;
        let denia_dir = plan.upper.join(".denia");
        create_runtime_directory(&denia_dir)?;
        self.chown_overlay_dir(&denia_dir)?;
        prepare_cgroup_directory(&self.cgroup_root, &plan.cgroup_path, CGROUP_CONTROLLERS)?;
        std::fs::write(
            plan.cgroup_path.join("cpu.max"),
            cpu_max(request.cpu_millis),
        )
        .map_err(path_io(
            "write cgroup cpu.max",
            plan.cgroup_path.join("cpu.max"),
        ))?;
        std::fs::write(
            plan.cgroup_path.join("memory.max"),
            format!("{}\n", request.memory_bytes),
        )
        .map_err(path_io(
            "write cgroup memory.max",
            plan.cgroup_path.join("memory.max"),
        ))?;
        if let Some(swap) = request.memory_swap_max {
            std::fs::write(
                plan.cgroup_path.join("memory.swap.max"),
                format!("{}\n", swap),
            )
            .map_err(path_io(
                "write cgroup memory.swap.max",
                plan.cgroup_path.join("memory.swap.max"),
            ))?;
        }
        if let Some(pids) = request.pids_max {
            std::fs::write(plan.cgroup_path.join("pids.max"), format!("{}\n", pids)).map_err(
                path_io("write cgroup pids.max", plan.cgroup_path.join("pids.max")),
            )?;
        }
        if let Some(weight) = request.io_weight {
            let _ = std::fs::write(plan.cgroup_path.join("io.weight"), format!("{}\n", weight));
        }
        Ok(())
    }

    pub fn cleanup(&self, plan: &LinuxRuntimePlan) -> Result<(), RuntimeError> {
        remove_cgroup_dir_if_exists(&plan.cgroup_path)?;
        let _ = rustix::mount::unmount(&plan.merged, rustix::mount::UnmountFlags::DETACH);
        // Remove the per-replica host socket dir; it lives under <data_dir>/sock
        // (outside replica_dir) so the wipe below won't catch it.
        if let Some(dir) = plan.socket_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        // The upper layer is ephemeral: wiping the replica dir removes
        // upper/work/merged so a relaunch starts from a clean writable layer.
        remove_dir_if_exists(&plan.replica_dir)?;
        Ok(())
    }

    /// Build the read-only bind mounts and (optional) loader-prefixed argv for
    /// running the daemon binary (socket-proxy / workload-launcher are the same
    /// multi-call binary) inside an arbitrary workload image.
    ///
    /// The daemon binary is bound read-only at `guest_target`; its host shared
    /// objects + dynamic loader (resolved from the daemon's own
    /// `/proc/self/maps`) are bound read-only under `SOCKET_PROXY_LIB_DIR`, and
    /// when dynamically linked the returned `loader_prefix` runs the helper
    /// through the bound loader with `--library-path` so it works regardless of
    /// the image's libc. A statically-linked daemon yields no libs and an empty
    /// prefix (the helper is exec'd directly). This replaces the previous
    /// `std::fs::copy` of the binary into the artifact rootfs, which mutated the
    /// shared content-addressed bundle and raced across concurrent same-digest
    /// runs (ADR-019: "the content-addressed bundle is never mutated"; M4).
    fn runtime_binary_binds(
        &self,
        guest_target: &str,
    ) -> Result<(Vec<RoBind>, Vec<String>), RuntimeError> {
        let helper_src = resolve_host_binary(&self.socket_proxy_source).ok_or_else(|| {
            RuntimeError::SocketProxyUnavailable {
                path: self.socket_proxy_source.clone(),
            }
        })?;
        let host_libs = if std::env::current_exe().ok().as_deref()
            == Some(self.socket_proxy_source.as_path())
        {
            socket_proxy_runtime_libs()
        } else {
            Vec::new()
        };
        let loader_guest = host_libs
            .iter()
            .find(|p| is_dynamic_loader(p))
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|name| format!("{SOCKET_PROXY_LIB_DIR}/{name}"));

        let mut loader_prefix = Vec::new();
        if let Some(loader) = &loader_guest {
            loader_prefix.push(loader.clone());
            loader_prefix.push("--library-path".to_string());
            loader_prefix.push(SOCKET_PROXY_LIB_DIR.to_string());
        }

        let mut ro_binds = vec![RoBind {
            src: helper_src,
            dest: PathBuf::from(guest_target),
        }];
        for lib in &host_libs {
            if let Some(name) = lib.file_name().and_then(|n| n.to_str()) {
                ro_binds.push(RoBind {
                    src: lib.clone(),
                    dest: PathBuf::from(format!("{SOCKET_PROXY_LIB_DIR}/{name}")),
                });
            }
        }
        Ok((ro_binds, loader_prefix))
    }

    fn prepare_socket_directory(&self, plan: &LinuxRuntimePlan) -> Result<(), RuntimeError> {
        // The socket lives on the REAL host fs (RW-bind-mounted into the guest at
        // the same absolute path, identity mount), NOT in the overlay upper — an AF_UNIX
        // socket on overlayfs is bound to the overlay inode and is not connectable
        // via the upperdir path. Create the host socket dir and chown it to the
        // userns base so the workload (mapped userns-root) can bind the socket;
        // the daemon connects to the same inode directly.
        if let Some(dir) = plan.socket_path.parent() {
            create_runtime_directory(dir)?;
            self.chown_overlay_dir(dir)?;
        }
        remove_existing_runtime_file(&plan.socket_path)?;
        Ok(())
    }

    /// Set a directory to mode 0755 so the workload's namespace-root (the host
    /// `userns_base` uid mapped to uid 0 inside the userns) can traverse it.
    /// Ownership is left unchanged: these are persistent ancestor dirs the
    /// daemon must keep writing into to create future deployments/replicas, and
    /// traversal only needs the `other` execute bit.
    fn set_traverse_mode(&self, path: &Path) -> Result<(), RuntimeError> {
        let mut perms = std::fs::metadata(path)
            .map_err(path_io("stat directory for permissions", path))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .map_err(path_io("set directory permissions to 0755", path))
    }

    /// Prepare an ephemeral overlay / guest-writable directory: set mode 0755
    /// and, when the process holds `CAP_CHOWN`, recursively chown it to
    /// `userns_base`.
    ///
    /// The overlay `upper` layer and the guest socket dirs are owned by the
    /// workload's namespace-root (host `userns_base`, which maps to uid 0 inside
    /// the userns) so the workload owns its writable layer and its runtime
    /// writes / copy-ups land with the guest's uid. (The overlay mount itself is
    /// now performed privileged in the initial user namespace — see
    /// `syscall::ns::child_prepare_root` / ADR-026 — so ownership is no longer
    /// required for the mount to succeed.) The chown is skipped when `CAP_CHOWN`
    /// is absent (unprivileged dev/test, where the privileged launch is not
    /// exercised); root and the daemon (ambient `CAP_CHOWN`, see
    /// `denia.service.in`) both have it.
    fn chown_overlay_dir(&self, path: &Path) -> Result<(), RuntimeError> {
        self.set_traverse_mode(path)?;
        if caps::has_effective_cap_chown() {
            chown::recursive_lchown(path, self.userns_base, self.userns_base)?;
        }
        Ok(())
    }
}

#[async_trait]
impl Runtime for LinuxRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.reap_exited_children()?;
        let plan = self.plan(&request)?;
        self.prepare(&plan, &request)?;
        let status_socket_path = plan.socket_path.clone();
        let connect_socket_path = plan.socket_connect_path.clone();
        let log_path = match self.service_log_path(request.service_id) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Io(error));
            }
        };
        let namespace = plan
            .namespace
            .clone()
            .with_stdio_paths(log_path.clone(), log_path);
        let pid = match tokio::task::spawn_blocking(move || spawn_namespaced_process(&namespace))
            .await?
        {
            Ok(pid) => Some(pid),
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Syscall(error));
            }
        };
        let status_cgroup_path = plan.cgroup_path.clone();
        // `spawn_namespaced_process` returns `Ok` only with a non-zero pid, so
        // `pid` is always `Some` here; map the impossible `None` to a typed error
        // rather than panicking on the launch path.
        let native_pid = match pid {
            Some(pid) => pid,
            None => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Syscall(SyscallError::ChildSetup {
                    stage: "spawn returned no pid",
                }));
            }
        };
        let mut process = TrackedProcess::NativePid(native_pid);
        if let Err(error) = wait_for_service_socket(&status_socket_path, &mut process).await {
            let mut tracked_child = TrackedChild { process, plan };
            let _ = terminate_tracked_child(&mut tracked_child).await;
            let _ = self.cleanup(&tracked_child.plan);
            return Err(error);
        }
        let tracked_child = TrackedChild { process, plan };
        let instance_id = request.instance_id();
        let insert_result = {
            match self.children.lock() {
                Ok(mut children) => Ok(children.insert(instance_id.clone(), tracked_child)),
                Err(_) => Err(tracked_child),
            }
        };
        let replaced_child = match insert_result {
            Ok(replaced_child) => replaced_child,
            Err(mut tracked_child) => {
                let _ = terminate_tracked_child(&mut tracked_child).await;
                let _ = self.cleanup(&tracked_child.plan);
                return Err(RuntimeError::LockPoisoned);
            }
        };
        if let Some(mut replaced_child) = replaced_child {
            let _ = terminate_tracked_child(&mut replaced_child).await;
            let _ = self.cleanup(&replaced_child.plan);
        }

        Ok(RuntimeStatus {
            service_id: request.service_id,
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid,
            cgroup_path: status_cgroup_path,
            socket_path: connect_socket_path,
            replica_index: request.replica_index,
        })
    }

    async fn run_to_completion(&self, request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        if request.artifact.kind != ArtifactKind::RootfsBundle {
            return Err(RuntimeError::UnsupportedArtifactKind {
                kind: request.artifact.kind.clone(),
            });
        }
        if request.cpu_millis == 0 {
            return Err(RuntimeError::InvalidResourceLimit {
                reason: "cpu_millis must be greater than zero".to_string(),
            });
        }
        if request.memory_bytes == 0 {
            return Err(RuntimeError::InvalidResourceLimit {
                reason: "memory_bytes must be greater than zero".to_string(),
            });
        }

        let bundle_dir = self
            .artifact_dir
            .join(safe_artifact_name(&request.artifact.digest));
        let rootfs_path = bundle_dir.join("rootfs");
        if !rootfs_path.exists() {
            return Err(RuntimeError::MissingRootfs { path: rootfs_path });
        }
        validate_runtime_directory(&rootfs_path)?;
        let manifest_path = bundle_dir.join("process.json");
        let manifest = std::fs::read_to_string(&manifest_path)?;
        let process: LinuxRuntimeProcessSpec = serde_json::from_str(&manifest)?;
        validate_process_spec(&process, &manifest_path)?;

        let argv = match request.command.clone() {
            Some(cmd) if !cmd.is_empty() => cmd,
            _ => process.argv.clone(),
        };
        if argv.is_empty() {
            return Err(RuntimeError::EmptyArgv {
                path: manifest_path,
            });
        }
        if !argv[0].starts_with('/') {
            return Err(RuntimeError::InvalidArgv {
                argv0: argv[0].clone(),
            });
        }

        let mut env_map: std::collections::BTreeMap<String, String> =
            process.env.into_iter().collect();
        for (key, value) in &request.env {
            env_map.insert(key.clone(), value.clone());
        }

        let run_dir = self
            .runtime_dir
            .join("jobs")
            .join(request.job_id.to_string())
            .join(request.run_id.to_string());
        let cgroup_path = self
            .cgroup_root
            .join("jobs")
            .join(request.job_id.to_string())
            .join(request.run_id.to_string());
        // Per-run overlay so the job — like a service replica — gets a private
        // writable layer over the shared, read-only artifact rootfs. The
        // workload-launcher is bind-mounted read-only into the guest instead of
        // being copied into the lower (which mutated the shared content-addressed
        // bundle and raced across concurrent same-digest runs). See ADR-019 /
        // ADR-026 / M4.
        let upper = run_dir.join("upper");
        let work = run_dir.join("work");
        let merged = run_dir.join("merged");
        std::fs::create_dir_all(&run_dir)?;
        // overlayfs requires an empty `work/work` for a fresh mount.
        let overlay_work = work.join("work");
        if overlay_work.exists() {
            std::fs::remove_dir_all(&overlay_work)
                .map_err(path_io("remove stale overlay work directory", &overlay_work))?;
        }
        create_dir_all("create job upper directory", &upper)?;
        create_dir_all("create job work directory", &work)?;
        create_dir_all("create job merged directory", &merged)?;
        // Persistent ancestor dirs need the traverse bit so the workload's mapped
        // userns-root can reach the read-only lower; the ephemeral overlay layers
        // are chowned to the userns base so guest writes/copy-ups are owned by the
        // workload. Mirrors the service `prepare` path.
        self.set_traverse_mode(&run_dir)?;
        self.chown_overlay_dir(&upper)?;
        self.chown_overlay_dir(&work)?;
        self.chown_overlay_dir(&merged)?;
        if let Some(data_dir) = self.artifact_dir.parent() {
            self.set_traverse_mode(data_dir)?;
        }
        self.set_traverse_mode(&self.runtime_dir)?;
        self.set_traverse_mode(&self.artifact_dir)?;
        if let Some(bundle) = rootfs_path.parent() {
            self.set_traverse_mode(bundle)?;
            self.set_traverse_mode(&rootfs_path)?;
        }
        let denia_dir = upper.join(".denia");
        create_runtime_directory(&denia_dir)?;
        self.chown_overlay_dir(&denia_dir)?;

        prepare_cgroup_directory(&self.cgroup_root, &cgroup_path, CGROUP_CONTROLLERS)?;
        // Resource-limit writes propagate errors (matching the service `prepare`
        // path): a failed `memory.max`/`pids.max` write would otherwise silently
        // launch the job unconstrained. Only `io.weight` stays best-effort, since
        // it is advisory (L3).
        std::fs::write(cgroup_path.join("cpu.max"), cpu_max(request.cpu_millis))
            .map_err(path_io("write cgroup cpu.max", cgroup_path.join("cpu.max")))?;
        std::fs::write(
            cgroup_path.join("memory.max"),
            format!("{}\n", request.memory_bytes),
        )
        .map_err(path_io(
            "write cgroup memory.max",
            cgroup_path.join("memory.max"),
        ))?;
        if let Some(swap) = request.memory_swap_max {
            std::fs::write(cgroup_path.join("memory.swap.max"), format!("{}\n", swap)).map_err(
                path_io(
                    "write cgroup memory.swap.max",
                    cgroup_path.join("memory.swap.max"),
                ),
            )?;
        }
        if let Some(pids) = request.pids_max {
            std::fs::write(cgroup_path.join("pids.max"), format!("{}\n", pids)).map_err(path_io(
                "write cgroup pids.max",
                cgroup_path.join("pids.max"),
            ))?;
        }
        if let Some(weight) = request.io_weight {
            let _ = std::fs::write(cgroup_path.join("io.weight"), format!("{}\n", weight));
        }

        let cleanup_merged = merged.clone();
        let cleanup = || {
            let _ = std::fs::write(cgroup_path.join("cgroup.kill"), "1\n");
            let _ = remove_cgroup_dir_if_exists(&cgroup_path);
            let _ = rustix::mount::unmount(&cleanup_merged, rustix::mount::UnmountFlags::DETACH);
            let _ = remove_dir_if_exists(&run_dir);
        };

        let overlay = OverlaySpec {
            lower: rootfs_path.clone(),
            upper: upper.clone(),
            work: work.clone(),
            merged: merged.clone(),
        };
        let (ro_binds, loader_prefix) = match self.runtime_binary_binds(WORKLOAD_LAUNCHER_TARGET) {
            Ok(binds) => binds,
            Err(error) => {
                cleanup();
                return Err(error);
            }
        };
        let mut child_argv = loader_prefix;
        child_argv.push(WORKLOAD_LAUNCHER_TARGET.to_string());
        child_argv.push("--".to_string());
        child_argv.extend(argv);
        let mut namespace = NamespaceConfig::new(merged.clone(), child_argv)
            .with_overlay(overlay)
            .with_uid_map(self.userns_base, self.userns_size)
            .with_cgroup_path(cgroup_path.clone())
            .with_workdir(process.workdir)
            .with_env(env_map.into_iter().collect());
        for bind in ro_binds {
            namespace = namespace.with_ro_bind(bind);
        }
        if let Some(pids) = request.pids_max {
            namespace = namespace.with_max_pids(Some(pids));
        }
        let started_at = chrono::Utc::now();
        let pid = match tokio::task::spawn_blocking(move || spawn_namespaced_process(&namespace))
            .await?
        {
            Ok(pid) => pid,
            Err(error) => {
                cleanup();
                return Err(RuntimeError::Syscall(error));
            }
        };
        let wait_status = match tokio::task::spawn_blocking(move || signal::wait(pid)).await? {
            Ok(status) => status,
            Err(error) => {
                cleanup();
                return Err(RuntimeError::Syscall(error));
            }
        };
        let finished_at = chrono::Utc::now();
        cleanup();

        Ok(JobOutcome {
            exit_code: exit_code_from_process_status(wait_status),
            started_at,
            finished_at,
        })
    }

    async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError> {
        validate_service_name(&instance.service_name)?;
        self.reap_exited_children()?;
        let mut tracked = {
            self.children
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?
                .remove(instance)
        };
        if let Some(tracked) = tracked.as_mut() {
            terminate_tracked_child(tracked).await?;
            self.cleanup(&tracked.plan)?;
        }
        Ok(())
    }

    async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError> {
        self.reap_exited_children()?;
        let children = self
            .children
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?;
        let statuses = children
            .iter()
            .map(|(instance, tracked)| {
                let TrackedProcess::NativePid(pid) = tracked.process;
                RuntimeStatus {
                    service_id: instance.service_id,
                    service_name: instance.service_name.clone(),
                    deployment_id: tracked.plan.deployment_id,
                    state: "running".to_string(),
                    pid: Some(pid),
                    cgroup_path: tracked.plan.cgroup_path.clone(),
                    socket_path: tracked.plan.socket_connect_path.clone(),
                    replica_index: instance.replica_index,
                }
            })
            .collect();
        Ok(statuses)
    }

    async fn sweep_orphans(&self) -> Result<usize, RuntimeError> {
        let mut swept = 0usize;
        let service_entries = match std::fs::read_dir(&self.runtime_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(RuntimeError::Io(error)),
        };
        // runtime_dir layout: {service_id}/{deployment_id}/{replica_index}/.
        // Names that don't parse as UUID/index (e.g. `jobs`) are skipped.
        for service_entry in service_entries.flatten() {
            let service_path = service_entry.path();
            let Some(service_id) = parse_uuid_dir(&service_path) else {
                continue;
            };
            let Ok(deployment_entries) = std::fs::read_dir(&service_path) else {
                continue;
            };
            for deployment_entry in deployment_entries.flatten() {
                let deployment_path = deployment_entry.path();
                let Some(deployment_id) = parse_uuid_dir(&deployment_path) else {
                    continue;
                };
                let Ok(replica_entries) = std::fs::read_dir(&deployment_path) else {
                    continue;
                };
                for replica_entry in replica_entries.flatten() {
                    let replica_path = replica_entry.path();
                    let Some(replica_index) = parse_index_dir(&replica_path) else {
                        continue;
                    };
                    self.sweep_one_replica(service_id, deployment_id, replica_index, &replica_path)
                        .await;
                    swept += 1;
                }
                // Prune the now-(hopefully)-empty deployment dir.
                let _ = std::fs::remove_dir(&deployment_path);
            }
            let _ = std::fs::remove_dir(&service_path);
        }
        // Remove socket aliases whose real socket no longer resolves (the deep
        // upper-side socket went away with the swept replica dir). See `plan`.
        self.sweep_socket_aliases();
        Ok(swept)
    }

    async fn open_console(
        &self,
        request: RuntimeConsoleRequest,
    ) -> Result<RuntimeConsoleSession, RuntimeError> {
        self.reap_exited_children()?;
        let instance = RuntimeInstanceId {
            service_id: request.service_id,
            service_name: request.service_name.clone(),
            replica_index: request.replica_index,
        };
        let tracked = {
            let children = self
                .children
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?;
            children
                .get(&instance)
                .ok_or_else(|| RuntimeError::InvalidServiceName {
                    name: "selected replica is not running".to_string(),
                })?
                .clone_for_console()
        };
        let TrackedProcess::NativePid(target_pid) = tracked.process;
        if target_pid == 0 {
            return Err(RuntimeError::InvalidServiceName {
                name: "selected replica has exited".to_string(),
            });
        }

        // Snapshot the replica's process start-time NOW (while it is confirmed
        // running under the children lock). The console child re-reads it after
        // joining the namespace fds and aborts if the pid was recycled onto a
        // different process — closing the PID-reuse TOCTOU on setns (review 07).
        let target_start_time = read_process_start_time(target_pid).ok_or_else(|| {
            RuntimeError::InvalidServiceName {
                name: "selected replica is no longer running".to_string(),
            }
        })?;

        let (pty, slave) = open_pty(request.cols, request.rows).map_err(RuntimeError::Io)?;
        let config = ConsoleLaunchConfig {
            target_pid,
            target_start_time,
            cgroup_path: tracked.plan.cgroup_path.clone(),
            workdir: tracked.plan.namespace.workdir.clone(),
            env: tracked.plan.namespace.env.clone(),
            shell: "/bin/sh".to_string(),
            // Reproduce the workload's per-launch privilege floor on the
            // interactive shell (ADR-005 / ADR-033). The service launcher always
            // runs hardened (NamespaceConfig defaults), so the console matches.
            no_new_privs: true,
            drop_bounding_caps: true,
            seccomp: true,
        };
        let child_pid = tokio::task::spawn_blocking(move || spawn_console_process(&config, slave))
            .await?
            .map_err(RuntimeError::Syscall)?;

        // Register the live console pid as a reaper backstop before handing the
        // session out, so a dropped bridge task never leaks a zombie.
        if let Ok(mut consoles) = self.console_children.lock() {
            consoles.push(child_pid);
        }

        Ok(RuntimeConsoleSession {
            session_id: request.session_id,
            replica_index: request.replica_index,
            child_pid,
            cgroup_path: tracked.plan.cgroup_path,
            pty: Box::new(pty),
            reaper: Some(Box::new(LinuxConsoleReaper {
                child_pid,
                console_children: self.console_children.clone(),
            })),
        })
    }
}

/// Console teardown handle handed to the API bridge. On `reap` it asks the shell
/// to exit (SIGTERM), waits a short grace period, escalates to SIGKILL if the
/// shell ignored SIGTERM, then `waitpid`s to collect the child and report how it
/// ended. Deregisters the pid from the runtime backstop once reaped.
struct LinuxConsoleReaper {
    child_pid: u32,
    console_children: Arc<Mutex<Vec<u32>>>,
}

impl LinuxConsoleReaper {
    /// Grace period between SIGTERM and the SIGKILL escalation for a shell that
    /// ignores SIGTERM (or is wedged). Bounded so a stuck session cannot hold the
    /// replica's namespaces open indefinitely after its websocket closes.
    const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

    fn deregister(&self) {
        if let Ok(mut consoles) = self.console_children.lock() {
            consoles.retain(|pid| *pid != self.child_pid);
        }
    }
}

#[async_trait]
impl crate::runtime::console::ConsoleReaper for LinuxConsoleReaper {
    async fn reap(&self) -> crate::runtime::ConsoleExit {
        use crate::runtime::ConsoleExit;
        use crate::syscall::signal::{self, ProcessStatus};
        use rustix::process::Signal;

        let pid = self.child_pid;
        // Ask the shell to exit.
        let _ = signal::kill(pid, Signal::TERM);

        // Poll for exit within the grace window; reap as soon as it is gone.
        let deadline = tokio::time::Instant::now() + Self::GRACE;
        let exit = loop {
            match signal::try_wait(pid) {
                Ok(ProcessStatus::Exited(code)) => break ConsoleExit::Code(code),
                Ok(ProcessStatus::Signaled(sig)) => break ConsoleExit::Signal(sig),
                // Still running, or wait failed (already reaped/ECHILD): decide
                // by the clock below.
                Ok(ProcessStatus::Running) => {}
                Err(_) => break ConsoleExit::Unknown,
            }
            if tokio::time::Instant::now() >= deadline {
                // Wedged or SIGTERM-ignoring shell: escalate and blocking-wait so
                // the zombie is collected and the namespaces are released.
                let _ = signal::kill(pid, Signal::KILL);
                break match signal::wait(pid) {
                    Ok(ProcessStatus::Exited(code)) => ConsoleExit::Code(code),
                    Ok(ProcessStatus::Signaled(sig)) => ConsoleExit::Signal(sig),
                    _ => ConsoleExit::Unknown,
                };
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };

        self.deregister();
        exit
    }
}

impl LinuxRuntime {
    /// Tear down one leftover replica found on disk: kill its cgroup, unmount
    /// the overlay, and remove the cgroup + replica directories. Best-effort —
    /// every step swallows its error (logged) so one stuck replica never aborts
    /// the rest of the boot sweep.
    async fn sweep_one_replica(
        &self,
        service_id: uuid::Uuid,
        deployment_id: uuid::Uuid,
        replica_index: u32,
        replica_path: &Path,
    ) {
        let cgroup_path = self
            .cgroup_root
            .join(service_id.to_string())
            .join(deployment_id.to_string())
            .join(replica_index.to_string());
        // 1. Kill any survivors in the cgroup (same backstop as normal stop).
        match std::fs::write(cgroup_path.join("cgroup.kill"), "1\n") {
            Ok(()) => self.wait_cgroup_drained(&cgroup_path).await,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                ?error,
                cgroup = %cgroup_path.display(),
                "orphan sweep: failed to write cgroup.kill"
            ),
        }
        // 2. Unmount the overlay mountpoint (may not be mounted; ignore errors).
        let merged = replica_path.join("merged");
        let _ = rustix::mount::unmount(&merged, rustix::mount::UnmountFlags::DETACH);
        // 3. Remove the cgroup leaf and the replica directory tree.
        if let Err(error) = remove_cgroup_dir_if_exists(&cgroup_path) {
            tracing::warn!(
                ?error,
                cgroup = %cgroup_path.display(),
                "orphan sweep: failed to remove cgroup dir"
            );
        }
        if let Err(error) = remove_dir_if_exists(replica_path) {
            tracing::warn!(
                ?error,
                replica = %replica_path.display(),
                "orphan sweep: failed to remove replica dir"
            );
        }
    }

    /// Poll `cgroup.procs` until it is empty (all killed pids reaped) or a short
    /// deadline passes, so the subsequent `rmdir` of the cgroup does not hit
    /// EBUSY. Best-effort and bounded.
    async fn wait_cgroup_drained(&self, cgroup_path: &Path) {
        let procs = cgroup_path.join("cgroup.procs");
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
        loop {
            match std::fs::read_to_string(&procs) {
                Ok(contents) if contents.split_whitespace().next().is_none() => return,
                Ok(_) => {}
                Err(_) => return,
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// Remove dangling socket-alias symlinks under `<data_dir>/sock`. An alias
    /// points at the deep upper-side socket of a replica; once that replica's
    /// dir is swept the alias dangles, so a target that no longer resolves marks
    /// a stale alias safe to delete. See `plan`'s `socket_connect_path`.
    fn sweep_socket_aliases(&self) {
        let sock_dir = self
            .runtime_dir
            .parent()
            .unwrap_or(self.runtime_dir.as_path())
            .join("sock");
        let Ok(entries) = std::fs::read_dir(&sock_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_symlink = std::fs::symlink_metadata(&path)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            // `metadata` follows the symlink; Err means the target is gone.
            if is_symlink && std::fs::metadata(&path).is_err() {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    fn reap_exited_children(&self) -> Result<(), RuntimeError> {
        self.reap_console_children();
        let exited = {
            let mut children = self
                .children
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?;
            let mut exited_instances = Vec::new();
            for (instance, tracked) in children.iter_mut() {
                if tracked.process.try_wait()? {
                    exited_instances.push(instance.clone());
                }
            }
            exited_instances
                .into_iter()
                .filter_map(|instance| children.remove(&instance))
                .collect::<Vec<_>>()
        };
        for tracked in exited {
            self.cleanup(&tracked.plan)?;
        }
        Ok(())
    }

    /// Backstop reaper for console-shell pids: non-blocking `try_wait` on each
    /// registered console child, dropping any that have exited (collecting the
    /// zombie). The bridge normally reaps its own child via [`LinuxConsoleReaper`];
    /// this only catches children whose bridge task was dropped/panicked. A pid
    /// the bridge already reaped reports `Err` (ECHILD) here and is also dropped.
    fn reap_console_children(&self) {
        let Ok(mut consoles) = self.console_children.lock() else {
            return;
        };
        consoles.retain(|pid| {
            matches!(
                crate::syscall::signal::try_wait(*pid),
                Ok(crate::syscall::signal::ProcessStatus::Running)
            )
        });
    }

    fn service_log_path(&self, service_id: uuid::Uuid) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.log_dir)?;
        // Log files are named by service_id (globally unique), not service.name
        // (unique only within a project), to prevent cross-project log mixing and
        // disclosure (F-3).
        let path = self.log_dir.join(format!("{service_id}.log"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CGROUP_CONTROLLERS, GUEST_SERVICE_SOCKET_ENV, LinuxRuntime, LinuxRuntimePlan,
        LinuxRuntimeProcessSpec, TrackedChild, TrackedProcess,
    };
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::domain::RuntimeStartRequest;
    use crate::runtime::error::RuntimeError;
    use crate::runtime::fs_helpers::{
        cgroup_has_requested_controllers, prepare_cgroup_directory, remove_cgroup_dir,
        remove_dir_if_exists, safe_artifact_name, terminate_tracked_child,
        terminate_tracked_process, wait_for_service_socket,
    };
    use crate::syscall::ns::NamespaceConfig;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    fn write_process_bundle(
        artifact_dir: &Path,
        digest: &str,
    ) -> (ArtifactRecord, PathBuf, PathBuf) {
        let artifact = ArtifactRecord::new(
            digest,
            ArtifactKind::RootfsBundle,
            ArtifactSource::ExternalRegistry {
                image: "test:latest".to_string(),
            },
        )
        .expect("artifact");
        let bundle_dir = artifact_dir.join(safe_artifact_name(digest));
        let rootfs = bundle_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).expect("rootfs dir");
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec(&LinuxRuntimeProcessSpec {
                argv: vec!["/bin/echo".to_string(), "hello".to_string()],
                env: Vec::new(),
                workdir: "/".to_string(),
            })
            .expect("manifest json"),
        )
        .expect("manifest");
        (artifact, bundle_dir, rootfs)
    }

    fn runtime_request(
        runtime_dir: &Path,
        artifact: ArtifactRecord,
        service_name: &str,
    ) -> RuntimeStartRequest {
        RuntimeStartRequest {
            service_name: service_name.to_string(),
            service_id: uuid::Uuid::now_v7(),
            deployment_id: uuid::Uuid::now_v7(),
            artifact,
            internal_port: 8080,
            socket_path: runtime_dir.join(service_name).join("current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        }
    }

    #[test]
    fn plan_distinct_per_replica_paths_with_upper_side_socket() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let runtime =
            LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir.clone(), cgroup_dir);
        let (artifact, _, _) = write_process_bundle(&artifact_dir, "sha256:replica");

        // Same service + deployment, two replica indices.
        let mut req0 = runtime_request(&runtime_dir, artifact, "replica-svc");
        let mut req1 = req0.clone();
        req0.replica_index = 0;
        req1.replica_index = 1;

        let plan0 = runtime.plan(&req0).expect("plan replica 0");
        let plan1 = runtime.plan(&req1).expect("plan replica 1");

        // Per-replica overlay layers, cgroup, and host socket path are all distinct.
        assert_ne!(plan0.upper, plan1.upper);
        assert_ne!(plan0.work, plan1.work);
        assert_ne!(plan0.merged, plan1.merged);
        assert_ne!(plan0.cgroup_path, plan1.cgroup_path);
        assert_ne!(plan0.socket_path, plan1.socket_path);

        // Host-side socket path is a short dir under <data_dir>/sock (real host
        // fs, RW-bind-mounted into the guest), NOT the overlay upper/merged nor
        // the read-only rootfs — an AF_UNIX socket on overlayfs isn't connectable
        // via the upperdir path. See ADR-026.
        assert!(
            plan0.socket_path.ends_with("service.sock"),
            "unexpected socket path: {}",
            plan0.socket_path.display()
        );
        assert!(
            plan0
                .socket_path
                .starts_with(runtime_dir.parent().unwrap().join("sock")),
            "socket path {} must be under <data_dir>/sock",
            plan0.socket_path.display()
        );
        for bad in [&plan0.upper, &plan0.merged, &plan0.rootfs_path] {
            assert!(
                !plan0.socket_path.starts_with(bad),
                "socket path must not be under {}",
                bad.display()
            );
        }
    }

    #[test]
    fn plan_includes_user_namespace_and_socket_proxy_stage() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let runtime =
            LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir.clone(), cgroup_dir)
                .with_userns(200000, 10000);

        let (artifact, _, _) = write_process_bundle(&artifact_dir, "sha256:abc");
        let request = runtime_request(&runtime_dir, artifact, "test-svc");

        let plan = runtime.plan(&request).expect("plan");

        // With overlay, the namespace root is the merged mountpoint; the lower is
        // the read-only artifact rootfs.
        assert_eq!(plan.namespace.rootfs, plan.merged);
        let overlay = plan.namespace.overlay.as_ref().expect("overlay set");
        assert_eq!(overlay.lower, plan.rootfs_path);
        assert_eq!(overlay.upper, plan.upper);
        assert_eq!(overlay.work, plan.work);
        assert_eq!(overlay.merged, plan.merged);
        assert_eq!(plan.namespace.workdir, "/");
        assert_eq!(
            plan.namespace.env,
            vec![(
                GUEST_SERVICE_SOCKET_ENV.to_string(),
                plan.socket_path.to_string_lossy().into_owned()
            )]
        );
        assert_eq!(plan.namespace.cgroup_path, plan.cgroup_path);
        assert_eq!(
            plan.namespace.uid_map,
            Some(crate::syscall::ns::UidMap {
                inside: 0,
                outside: 200000,
                size: 10000,
            })
        );
        assert_eq!(plan.namespace.uid_map, plan.namespace.gid_map);
        // argv is the socket-proxy invocation, optionally prefixed by the host
        // dynamic loader (`<loader> --library-path /.denia/lib`) when the
        // socket-proxy binary is dynamically linked (it is, in tests).
        let argv = &plan.namespace.argv;
        let i = argv
            .iter()
            .position(|a| a == "/.denia/socket-proxy")
            .expect("socket-proxy present in argv");
        let expected_tail = vec![
            "/.denia/socket-proxy".to_string(),
            "--listen".to_string(),
            plan.socket_path.to_string_lossy().into_owned(),
            "--connect".to_string(),
            "127.0.0.1:8080".to_string(),
            "--".to_string(),
            "/bin/echo".to_string(),
            "hello".to_string(),
        ];
        assert_eq!(&argv[i..], expected_tail.as_slice());
        if i > 0 {
            assert_eq!(argv[1], "--library-path");
            assert_eq!(argv[2], "/.denia/lib");
            assert!(
                argv[0].starts_with("/.denia/lib/ld-"),
                "expected a bound host loader as argv[0], got {:?}",
                argv[0]
            );
            // The loader + libs named in --library-path must have matching binds.
            let lib_binds = plan
                .namespace
                .ro_binds
                .iter()
                .filter(|b| b.dest.starts_with("/.denia/lib"))
                .count();
            assert!(lib_binds >= 1, "expected host-lib read-only binds");
        }
    }

    #[test]
    fn plan_rejects_rootfs_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, rootfs) = write_process_bundle(&artifact_dir, "sha256:link");
        std::fs::remove_dir_all(&rootfs).expect("remove rootfs dir");
        symlink(tmp.path(), &rootfs).expect("rootfs symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");

        let error = runtime.plan(&request).expect_err("rootfs symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &rootfs),
            "expected unsafe rootfs path, got: {error:?}"
        );
    }

    #[test]
    fn prepare_does_not_write_into_readonly_rootfs() {
        // The artifact rootfs is the overlay lower (read-only). prepare must not
        // create `.denia`, `run/denia`, or copy the socket-proxy into it; those
        // now live in the per-replica upper layer and a read-only bind mount.
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:ro-rootfs");
        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");

        runtime.prepare(&plan, &request).expect("prepare");

        assert!(
            !rootfs.join(".denia").exists(),
            "prepare must not inject helpers into the read-only rootfs"
        );
        assert!(
            !rootfs.join("run/denia").exists(),
            "prepare must not create the socket directory in the read-only rootfs"
        );
        assert!(
            plan.socket_path.parent().unwrap().is_dir(),
            "prepare must create the host-side socket directory"
        );
        assert!(
            plan.upper.join(".denia").is_dir(),
            "prepare must create the .denia directory in the per-replica upper layer"
        );
    }

    #[test]
    fn prepare_removes_stale_service_socket_before_launch() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, _rootfs) =
            write_process_bundle(&artifact_dir, "sha256:stale-socket");
        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");
        std::fs::create_dir_all(plan.socket_path.parent().unwrap()).expect("socket dir");
        std::fs::write(&plan.socket_path, "stale").expect("stale socket marker");

        runtime.prepare(&plan, &request).expect("prepare");

        assert!(
            !plan.socket_path.exists(),
            "prepare must remove a stale service socket before readiness waits"
        );
    }

    #[test]
    fn prepare_rejects_service_socket_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, _rootfs) =
            write_process_bundle(&artifact_dir, "sha256:socket-link");
        let outside_socket = tmp.path().join("outside-socket");
        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");
        std::fs::create_dir_all(plan.socket_path.parent().unwrap()).expect("socket dir");
        symlink(&outside_socket, &plan.socket_path).expect("socket symlink");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("service socket symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &plan.socket_path),
            "expected unsafe service socket path, got: {error:?}"
        );
        assert!(
            !outside_socket.exists(),
            "prepare must not remove or write through a socket symlink"
        );
    }

    #[test]
    fn plan_rejects_missing_socket_proxy_source() {
        // The socket-proxy is resolved at plan time (it is bound read-only into
        // the guest), so a missing source surfaces from `plan`, not `prepare`.
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let missing_proxy = tmp.path().join("missing-socket-proxy");

        let (artifact, _bundle_dir, _) =
            write_process_bundle(&artifact_dir, "sha256:missing-proxy");
        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir)
            .with_socket_proxy(&missing_proxy);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");

        let error = runtime
            .plan(&request)
            .expect_err("missing socket proxy rejected");

        assert!(
            matches!(error, RuntimeError::SocketProxyUnavailable { ref path } if path == &missing_proxy),
            "expected missing socket proxy source, got: {error:?}"
        );
    }

    #[test]
    fn prepare_cgroup_directory_enables_controllers_on_parents() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("cgroup");
        let service = root.join("service");
        let deployment = service.join("deployment");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("cgroup.controllers"), "cpu memory pids io\n")
            .expect("controllers");
        std::fs::write(root.join("cgroup.subtree_control"), "").expect("subtree");
        std::fs::create_dir_all(&service).expect("service");
        std::fs::write(service.join("cgroup.controllers"), "cpu memory pids io\n")
            .expect("controllers");
        std::fs::write(service.join("cgroup.subtree_control"), "").expect("subtree");

        prepare_cgroup_directory(&root, &deployment, CGROUP_CONTROLLERS).expect("prepare cgroup");

        assert_eq!(
            std::fs::read_to_string(root.join("cgroup.subtree_control")).expect("root subtree"),
            "+cpu +memory +pids +io\n"
        );
        assert_eq!(
            std::fs::read_to_string(service.join("cgroup.subtree_control"))
                .expect("service subtree"),
            "+cpu +memory +pids +io\n"
        );
        assert!(deployment.exists());
    }

    #[test]
    fn prepare_cgroup_directory_skips_non_cgroup_temp_dirs() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().join("plain-cgroup");
        let deployment = root.join("service").join("deployment");

        prepare_cgroup_directory(&root, &deployment, CGROUP_CONTROLLERS).expect("prepare cgroup");

        assert!(deployment.exists());
    }

    #[test]
    fn cgroup_has_requested_controllers_requires_every_controller() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cgroup = tmp.path().join("cgroup");
        std::fs::create_dir(&cgroup).expect("cgroup dir");
        std::fs::write(cgroup.join("cgroup.controllers"), "cpu\n").expect("controllers");

        assert!(!cgroup_has_requested_controllers(&cgroup, CGROUP_CONTROLLERS).expect("check"));
        assert!(cgroup_has_requested_controllers(&cgroup, &["cpu"]).expect("check"));
    }

    #[tokio::test]
    async fn terminate_tracked_child_uses_cgroup_kill_when_available() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cgroup_path = tmp.path().join("cgroup");
        std::fs::create_dir_all(&cgroup_path).expect("cgroup dir");
        std::fs::write(cgroup_path.join("cgroup.kill"), "").expect("cgroup.kill");
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep child");
        let pid = child.id();
        let mut tracked = TrackedChild {
            process: TrackedProcess::NativePid(pid),
            plan: LinuxRuntimePlan {
                namespace: NamespaceConfig::new(
                    tmp.path().join("rootfs"),
                    vec!["/bin/true".to_string()],
                )
                .with_uid_map(100000, 65536)
                .with_cgroup_path(cgroup_path.clone()),
                rootfs_path: tmp.path().join("rootfs"),
                socket_path: tmp.path().join("replica/upper/run/denia/service.sock"),
                socket_connect_path: tmp.path().join("sock/test.sock"),
                guest_socket_path: tmp
                    .path()
                    .join("sock/test.sock")
                    .to_string_lossy()
                    .into_owned(),
                cgroup_path: cgroup_path.clone(),
                deployment_id: uuid::Uuid::now_v7(),
                service_dir: tmp.path().join("runtime/test-svc"),
                deployment_dir: tmp.path().join("runtime/test-svc/deployment"),
                replica_dir: tmp.path().join("runtime/test-svc/deployment/0"),
                upper: tmp.path().join("runtime/test-svc/deployment/0/upper"),
                work: tmp.path().join("runtime/test-svc/deployment/0/work"),
                merged: tmp.path().join("runtime/test-svc/deployment/0/merged"),
                artifact_dir: tmp.path().join("artifacts"),
                runtime_dir: tmp.path().join("runtime"),
            },
        };

        terminate_tracked_child(&mut tracked)
            .await
            .expect("terminate");

        assert_eq!(
            std::fs::read_to_string(cgroup_path.join("cgroup.kill")).expect("cgroup.kill"),
            "1\n"
        );
        assert!(
            matches!(tracked.process, TrackedProcess::NativePid(0)),
            "child should be reaped after termination"
        );
        std::mem::forget(child);
    }

    #[tokio::test]
    async fn wait_for_service_socket_returns_when_socket_path_appears() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let socket_path = tmp.path().join("service.sock");
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep child");
        let mut process = TrackedProcess::NativePid(child.id());
        let create_path = socket_path.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _listener =
                std::os::unix::net::UnixListener::bind(create_path).expect("bind socket marker");
        });

        wait_for_service_socket(&socket_path, &mut process)
            .await
            .expect("service socket ready");

        assert!(socket_path.exists());
        assert!(
            matches!(process, TrackedProcess::NativePid(pid) if pid != 0),
            "process should still be tracked after readiness"
        );
        let _ = terminate_tracked_process(&mut process).await;
        std::mem::forget(child);
    }

    #[tokio::test]
    async fn wait_for_service_socket_errors_when_launcher_exits_first() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let socket_path = tmp.path().join("service.sock");
        let child = std::process::Command::new("true")
            .spawn()
            .expect("true child");
        let mut process = TrackedProcess::NativePid(child.id());

        let error = wait_for_service_socket(&socket_path, &mut process)
            .await
            .expect_err("socket should not be ready");

        assert!(
            matches!(error, RuntimeError::ServiceSocketUnavailable { ref path } if path == &socket_path),
            "expected service socket unavailable, got: {error:?}"
        );
        assert!(
            matches!(process, TrackedProcess::NativePid(0)),
            "process should be marked reaped"
        );
        std::mem::forget(child);
    }

    #[test]
    fn remove_dir_if_exists_removes_normal_runtime_tree() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("runtime/service/deployment");
        std::fs::create_dir_all(path.join("logs")).expect("runtime dirs");
        std::fs::write(path.join("logs/stdout.log"), "hello").expect("runtime file");

        remove_dir_if_exists(&path).expect("remove runtime tree");

        assert!(!path.exists());
    }

    #[test]
    fn remove_cgroup_dir_removes_nested_empty_cgroup_dirs() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("cgroup/service/deployment");
        std::fs::create_dir_all(path.join("child")).expect("cgroup dirs");

        remove_cgroup_dir(&path).expect("remove cgroup tree");

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn open_console_rejects_missing_replica() {
        use crate::runtime::Runtime;
        use crate::runtime::console::RuntimeConsoleRequest;

        let runtime = LinuxRuntime::new_with_paths(
            tempfile::tempdir().unwrap().path().join("runtime"),
            tempfile::tempdir().unwrap().path().join("artifacts"),
            tempfile::tempdir().unwrap().path().join("cgroup"),
        );
        let err = runtime
            .open_console(RuntimeConsoleRequest {
                session_id: uuid::Uuid::now_v7(),
                service_id: uuid::Uuid::now_v7(),
                service_name: "web".to_string(),
                deployment_id: uuid::Uuid::now_v7(),
                replica_index: 0,
                cols: 120,
                rows: 32,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("selected replica"));
    }
}
