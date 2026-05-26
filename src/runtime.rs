use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::artifacts::ArtifactKind;
use crate::domain::{JobOutcome, JobRunRequest, RuntimeStartRequest, RuntimeStatus};
use crate::syscall::ns::{NamespaceConfig, spawn_namespaced_process};
use crate::syscall::signal::{self, ProcessStatus};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime lock poisoned")]
    LockPoisoned,
    #[error("invalid runtime service name: {name}")]
    InvalidServiceName { name: String },
    #[error("linux runtime requires a rootfs bundle artifact, got {kind:?}")]
    UnsupportedArtifactKind { kind: ArtifactKind },
    #[error("runtime process manifest is missing argv: {path}")]
    EmptyArgv { path: PathBuf },
    #[error("runtime process argv[0] must be an absolute path: {argv0}")]
    InvalidArgv { argv0: String },
    #[error("runtime process workdir must be absolute: {workdir}")]
    InvalidWorkdir { workdir: String },
    #[error("runtime process environment key is invalid: {key}")]
    InvalidEnvironmentKey { key: String },
    #[error("rootfs bundle is missing: {path}")]
    MissingRootfs { path: PathBuf },
    #[error("runtime path is unsafe: {path}")]
    UnsafeRuntimePath { path: PathBuf },
    #[error("invalid runtime resource limit: {reason}")]
    InvalidResourceLimit { reason: String },
    #[error("namespace launcher binary not found: {path}")]
    NamespaceLauncherUnavailable { path: PathBuf },
    #[error("socket proxy binary not found: {path}")]
    SocketProxyUnavailable { path: PathBuf },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest json error: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("syscall error: {0}")]
    Syscall(#[from] crate::syscall::SyscallError),
    #[error("native runtime wait task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError>;
    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError>;
    async fn run_to_completion(&self, _request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        Err(RuntimeError::InvalidServiceName {
            name: "run_to_completion not implemented".to_string(),
        })
    }
}

#[async_trait]
impl<T> Runtime for Arc<T>
where
    T: Runtime + ?Sized,
{
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        (**self).start(request).await
    }

    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError> {
        (**self).stop(service_name).await
    }

    async fn run_to_completion(&self, request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        (**self).run_to_completion(request).await
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeRuntime {
    started: Arc<Mutex<Vec<RuntimeStartRequest>>>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl FakeRuntime {
    pub fn started_requests(&self) -> Vec<RuntimeStartRequest> {
        self.started.lock().expect("started lock").clone()
    }

    pub fn stopped_services(&self) -> Vec<String> {
        self.stopped.lock().expect("stopped lock").clone()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.started
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(request.clone());
        Ok(RuntimeStatus {
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid: Some(1234),
            cgroup_path: "/sys/fs/cgroup/denia/fake".into(),
            socket_path: request.socket_path,
        })
    }

    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError> {
        self.stopped
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(service_name.to_string());
        Ok(())
    }

    async fn run_to_completion(&self, _request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        let now = chrono::Utc::now();
        Ok(JobOutcome {
            exit_code: 0,
            started_at: now,
            finished_at: now,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxRuntimeProcessSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxRuntimePlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub namespace: NamespaceConfig,
    pub rootfs_path: PathBuf,
    pub socket_path: PathBuf,
    pub guest_socket_path: String,
    pub cgroup_path: PathBuf,
    pub service_dir: PathBuf,
    pub deployment_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LinuxRuntime {
    runtime_dir: PathBuf,
    artifact_dir: PathBuf,
    cgroup_root: PathBuf,
    log_dir: PathBuf,
    unshare_binary: PathBuf,
    socket_proxy_source: PathBuf,
    userns_base: u32,
    userns_size: u32,
    children: Arc<Mutex<HashMap<String, TrackedChild>>>,
}

const SOCKET_PROXY_TARGET: &str = "/.denia/socket-proxy";
const WORKLOAD_LAUNCHER_TARGET: &str = "/.denia/workload-launcher";
const GUEST_SERVICE_SOCKET: &str = "/run/denia/service.sock";
const GUEST_SERVICE_SOCKET_ENV: &str = "DENIA_SERVICE_SOCKET";

#[derive(Debug)]
struct TrackedChild {
    process: TrackedProcess,
    plan: LinuxRuntimePlan,
}

#[derive(Debug)]
enum TrackedProcess {
    NativePid(u32),
}

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
        Self::new_with_paths_and_launcher(runtime_dir, artifact_dir, cgroup_root, "unshare")
    }

    pub fn new_with_paths_and_launcher(
        runtime_dir: impl Into<PathBuf>,
        artifact_dir: impl Into<PathBuf>,
        cgroup_root: impl Into<PathBuf>,
        unshare_binary: impl Into<PathBuf>,
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
            unshare_binary: unshare_binary.into(),
            socket_proxy_source: std::env::current_exe().unwrap_or_else(|_| "denia".into()),
            userns_base: 100000,
            userns_size: 65536,
            children: Arc::new(Mutex::new(HashMap::new())),
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
        env_map.insert(
            GUEST_SERVICE_SOCKET_ENV.to_string(),
            GUEST_SERVICE_SOCKET.to_string(),
        );
        let env: Vec<(String, String)> = env_map.into_iter().collect();

        let service_dir = self.runtime_dir.join(request.service_id.to_string());
        let deployment_dir = service_dir.join(request.deployment_id.to_string());
        let socket_path = rootfs_path.join(GUEST_SERVICE_SOCKET.trim_start_matches('/'));
        let cgroup_path = self
            .cgroup_root
            .join(request.service_id.to_string())
            .join(request.deployment_id.to_string());
        let mut child_argv = vec![
            SOCKET_PROXY_TARGET.to_string(),
            "--listen".to_string(),
            GUEST_SERVICE_SOCKET.to_string(),
            "--connect".to_string(),
            format!("127.0.0.1:{}", request.internal_port),
            "--".to_string(),
        ];
        child_argv.extend(process.argv);
        let namespace = NamespaceConfig::new(rootfs_path.clone(), child_argv.clone())
            .with_uid_map(self.userns_base, self.userns_size)
            .with_cgroup_path(cgroup_path.clone())
            .with_workdir(process.workdir.clone())
            .with_env(env.clone());

        let mut args = vec![
            "--user".to_string(),
            format!("--map-users=0,{},{}", self.userns_base, self.userns_size),
            format!("--map-groups=0,{},{}", self.userns_base, self.userns_size),
            "--fork".to_string(),
            "--pid".to_string(),
            "--net".to_string(),
            "--mount".to_string(),
            "--propagation".to_string(),
            "private".to_string(),
            "--uts".to_string(),
            "--ipc".to_string(),
            "--mount-proc".to_string(),
            "--root".to_string(),
            rootfs_path.display().to_string(),
            "--wd".to_string(),
            process.workdir,
            "--".to_string(),
        ];
        args.extend(child_argv);

        Ok(LinuxRuntimePlan {
            program: self.unshare_binary.clone(),
            args,
            env,
            namespace,
            rootfs_path,
            socket_path,
            guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
            cgroup_path,
            service_dir,
            deployment_dir,
        })
    }

    pub fn prepare(
        &self,
        plan: &LinuxRuntimePlan,
        request: &RuntimeStartRequest,
    ) -> Result<(), RuntimeError> {
        validate_resource_limits(request)?;
        self.inject_socket_proxy(&plan.rootfs_path)?;
        self.prepare_socket_directory(plan)?;
        std::fs::create_dir_all(&plan.deployment_dir)?;
        std::fs::create_dir_all(&plan.cgroup_path)?;
        std::fs::write(
            plan.cgroup_path.join("cpu.max"),
            cpu_max(request.cpu_millis),
        )?;
        std::fs::write(
            plan.cgroup_path.join("memory.max"),
            format!("{}\n", request.memory_bytes),
        )?;
        Ok(())
    }

    pub fn cleanup(&self, plan: &LinuxRuntimePlan) -> Result<(), RuntimeError> {
        remove_dir_if_exists(&plan.cgroup_path)?;
        remove_dir_if_exists(&plan.deployment_dir)?;
        Ok(())
    }

    fn inject_socket_proxy(&self, rootfs: &Path) -> Result<(), RuntimeError> {
        self.inject_runtime_binary(rootfs, SOCKET_PROXY_TARGET)
    }

    fn inject_workload_launcher(&self, rootfs: &Path) -> Result<(), RuntimeError> {
        self.inject_runtime_binary(rootfs, WORKLOAD_LAUNCHER_TARGET)
    }

    fn inject_runtime_binary(&self, rootfs: &Path, target_path: &str) -> Result<(), RuntimeError> {
        let proxy_source = resolve_host_binary(&self.socket_proxy_source).ok_or_else(|| {
            RuntimeError::SocketProxyUnavailable {
                path: self.socket_proxy_source.clone(),
            }
        })?;
        let target_dir = rootfs.join(".denia");
        create_runtime_directory(&target_dir)?;
        let target = rootfs.join(target_path.trim_start_matches('/'));
        remove_existing_runtime_file(&target)?;
        std::fs::copy(&proxy_source, &target)?;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)?;
        Ok(())
    }

    fn prepare_socket_directory(&self, plan: &LinuxRuntimePlan) -> Result<(), RuntimeError> {
        create_runtime_directory(&plan.rootfs_path.join("run"))?;
        create_runtime_directory(&plan.rootfs_path.join("run/denia"))?;
        Ok(())
    }
}

fn resolve_host_binary(source: &Path) -> Option<PathBuf> {
    if source.is_absolute() || source.to_string_lossy().contains('/') {
        if source.exists() {
            return Some(source.to_path_buf());
        }
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(source);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[async_trait]
impl Runtime for LinuxRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.reap_exited_children()?;
        let plan = self.plan(&request)?;
        self.prepare(&plan, &request)?;
        let status_socket_path = plan.socket_path.clone();
        let log_path = match self.service_log_path(&request.service_name) {
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
        let pid = match spawn_namespaced_process(&namespace) {
            Ok(pid) => Some(pid),
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Syscall(error));
            }
        };
        let status_cgroup_path = plan.cgroup_path.clone();
        let tracked_child = TrackedChild {
            process: TrackedProcess::NativePid(pid.expect("native pid")),
            plan,
        };
        let insert_result = {
            match self.children.lock() {
                Ok(mut children) => {
                    Ok(children.insert(request.service_name.clone(), tracked_child))
                }
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
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid,
            cgroup_path: status_cgroup_path,
            socket_path: status_socket_path,
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
        self.inject_workload_launcher(&rootfs_path)?;
        std::fs::create_dir_all(&run_dir)?;
        std::fs::create_dir_all(&cgroup_path)?;
        std::fs::write(cgroup_path.join("cpu.max"), cpu_max(request.cpu_millis))?;
        std::fs::write(
            cgroup_path.join("memory.max"),
            format!("{}\n", request.memory_bytes),
        )?;

        let cleanup = || {
            let _ = std::fs::write(cgroup_path.join("cgroup.kill"), "1\n");
            let _ = remove_dir_if_exists(&cgroup_path);
            let _ = remove_dir_if_exists(&run_dir);
        };

        let mut child_argv = vec![WORKLOAD_LAUNCHER_TARGET.to_string(), "--".to_string()];
        child_argv.extend(argv);
        let namespace = NamespaceConfig::new(rootfs_path.clone(), child_argv)
            .with_uid_map(self.userns_base, self.userns_size)
            .with_cgroup_path(cgroup_path.clone())
            .with_workdir(process.workdir)
            .with_env(env_map.into_iter().collect());
        let started_at = chrono::Utc::now();
        let pid = match spawn_namespaced_process(&namespace) {
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

    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError> {
        validate_service_name(service_name)?;
        self.reap_exited_children()?;
        let mut tracked = {
            self.children
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?
                .remove(service_name)
        };
        if let Some(tracked) = tracked.as_mut() {
            terminate_tracked_child(tracked).await?;
            self.cleanup(&tracked.plan)?;
        }
        Ok(())
    }
}

impl LinuxRuntime {
    fn reap_exited_children(&self) -> Result<(), RuntimeError> {
        let exited = {
            let mut children = self
                .children
                .lock()
                .map_err(|_| RuntimeError::LockPoisoned)?;
            let mut service_names = Vec::new();
            for (service_name, tracked) in children.iter_mut() {
                if tracked.process.try_wait()? {
                    service_names.push(service_name.clone());
                }
            }
            service_names
                .into_iter()
                .filter_map(|service_name| children.remove(&service_name))
                .collect::<Vec<_>>()
        };
        for tracked in exited {
            self.cleanup(&tracked.plan)?;
        }
        Ok(())
    }

    fn service_log_path(&self, service_name: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.log_dir)?;
        let path = self.log_dir.join(format!("{service_name}.log"));
        OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(path)
    }
}

fn validate_service_name(service_name: &str) -> Result<(), RuntimeError> {
    let valid = !service_name.is_empty()
        && service_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidServiceName {
            name: service_name.to_string(),
        })
    }
}

fn validate_process_spec(
    process: &LinuxRuntimeProcessSpec,
    manifest_path: &Path,
) -> Result<(), RuntimeError> {
    if process.argv.is_empty() {
        return Err(RuntimeError::EmptyArgv {
            path: manifest_path.to_path_buf(),
        });
    }
    if !process.argv[0].starts_with('/') {
        return Err(RuntimeError::InvalidArgv {
            argv0: process.argv[0].clone(),
        });
    }
    if !process.workdir.starts_with('/') {
        return Err(RuntimeError::InvalidWorkdir {
            workdir: process.workdir.clone(),
        });
    }
    for (key, _) in &process.env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(RuntimeError::InvalidEnvironmentKey { key: key.clone() });
        }
    }
    Ok(())
}

fn validate_resource_limits(request: &RuntimeStartRequest) -> Result<(), RuntimeError> {
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
    Ok(())
}

fn safe_artifact_name(digest: &str) -> String {
    digest
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn cpu_max(cpu_millis: u32) -> String {
    format!("{} 100000\n", u64::from(cpu_millis) * 100)
}

fn remove_dir_if_exists(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::Io(error)),
    }
}

async fn terminate_tracked_child(tracked: &mut TrackedChild) -> Result<(), RuntimeError> {
    match std::fs::write(tracked.plan.cgroup_path.join("cgroup.kill"), "1\n") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Io(error)),
    }
    terminate_tracked_process(&mut tracked.process).await?;
    Ok(())
}

async fn terminate_tracked_process(process: &mut TrackedProcess) -> Result<(), RuntimeError> {
    match process {
        TrackedProcess::NativePid(pid) => {
            if *pid == 0 {
                return Ok(());
            }
            let raw_pid = *pid;
            if signal::try_wait(raw_pid)? == ProcessStatus::Running {
                signal::kill(raw_pid, rustix::process::Signal::KILL)?;
                let _ = tokio::task::spawn_blocking(move || signal::wait(raw_pid)).await??;
            }
            *pid = 0;
        }
    }
    Ok(())
}

impl TrackedProcess {
    fn try_wait(&mut self) -> Result<bool, RuntimeError> {
        match self {
            TrackedProcess::NativePid(pid) => {
                if *pid == 0 {
                    return Ok(true);
                }
                match signal::try_wait(*pid)? {
                    ProcessStatus::Running => Ok(false),
                    ProcessStatus::Exited(_) | ProcessStatus::Signaled(_) => {
                        *pid = 0;
                        Ok(true)
                    }
                }
            }
        }
    }
}

fn validate_runtime_directory(path: &Path) -> Result<(), RuntimeError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RuntimeError::UnsafeRuntimePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn create_runtime_directory(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RuntimeError::UnsafeRuntimePath {
                    path: path.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)?;
            validate_runtime_directory(path)?;
        }
        Err(error) => return Err(RuntimeError::Io(error)),
    }
    Ok(())
}

fn remove_existing_runtime_file(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || metadata.is_dir() {
                return Err(RuntimeError::UnsafeRuntimePath {
                    path: path.to_path_buf(),
                });
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Io(error)),
    }
    Ok(())
}

fn exit_code_from_process_status(status: ProcessStatus) -> i32 {
    match status {
        ProcessStatus::Running => -1,
        ProcessStatus::Exited(code) => code,
        ProcessStatus::Signaled(signal) => 128 + signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::domain::RuntimeStartRequest;
    use std::os::unix::fs::symlink;

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

        let args = &plan.args;
        assert_eq!(plan.namespace.rootfs, plan.rootfs_path);
        assert_eq!(plan.namespace.workdir, "/");
        assert_eq!(plan.namespace.env, plan.env);
        assert_eq!(plan.namespace.cgroup_path, plan.cgroup_path);
        assert_eq!(
            plan.namespace.argv,
            vec![
                "/.denia/socket-proxy".to_string(),
                "--listen".to_string(),
                "/run/denia/service.sock".to_string(),
                "--connect".to_string(),
                "127.0.0.1:8080".to_string(),
                "--".to_string(),
                "/bin/echo".to_string(),
                "hello".to_string(),
            ]
        );

        let user_pos = args
            .iter()
            .position(|a| a == "--user")
            .expect("--user flag present");

        let map_users_idx = args
            .iter()
            .position(|a| a.contains("--map-users="))
            .expect("--map-users= flag present");
        assert!(map_users_idx > user_pos, "--map-users after --user");

        assert!(
            args.contains(&"--map-users=0,200000,10000".to_string()),
            "expected --map-users=0,200000,10000, got: {:?}",
            args.iter()
                .filter(|a| a.contains("map-users"))
                .collect::<Vec<_>>()
        );
        assert!(
            args.contains(&"--map-groups=0,200000,10000".to_string()),
            "expected --map-groups=0,200000,10000"
        );

        assert_eq!(plan.program, PathBuf::from("unshare"));
        assert_eq!(args[0], "--user");
        let sep_pos = args
            .iter()
            .position(|a| a == "/.denia/socket-proxy")
            .expect("socket proxy present");
        assert!(
            args[sep_pos - 1] == "--",
            "socket proxy follows compatibility separator"
        );
        assert!(args[sep_pos + 1] == "--listen", "proxy listen flag");
        assert!(
            args[sep_pos + 2] == "/run/denia/service.sock",
            "proxy listens on guest service socket"
        );
        assert!(args[sep_pos + 3] == "--connect", "proxy connect flag");
        assert!(
            args[sep_pos + 4] == "127.0.0.1:8080",
            "proxy connects to the service internal port"
        );
        assert!(args[sep_pos + 5] == "--", "proxy child separator present");
        assert!(
            args[sep_pos + 6] == "/bin/echo",
            "workload argv follows proxy separator"
        );
        assert!(args[sep_pos + 7] == "hello", "workload arg follows argv[0]");
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
    fn plan_keeps_namespace_launcher_as_compatibility_payload() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let missing_launcher = tmp.path().join("missing-unshare");
        let runtime = LinuxRuntime::new_with_paths_and_launcher(
            runtime_dir.clone(),
            artifact_dir.clone(),
            cgroup_dir,
            &missing_launcher,
        );
        let (artifact, _, _) = write_process_bundle(&artifact_dir, "sha256:missing-unshare");
        let request = runtime_request(&runtime_dir, artifact, "test-svc");

        let plan = runtime.plan(&request).expect("plan");

        assert_eq!(plan.program, missing_launcher);
    }

    #[test]
    fn prepare_rejects_denia_directory_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:denia-link");
        let outside_dir = tmp.path().join("outside-denia");
        std::fs::create_dir_all(&outside_dir).expect("outside dir");
        symlink(&outside_dir, rootfs.join(".denia")).expect(".denia symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err(".denia symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &rootfs.join(".denia")),
            "expected unsafe .denia path, got: {error:?}"
        );
        assert!(
            !outside_dir.join("socket-proxy").exists(),
            "prepare must not copy runtime helpers outside the rootfs"
        );
    }

    #[test]
    fn prepare_rejects_socket_run_directory_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:run-link");
        let outside_run = tmp.path().join("outside-run");
        std::fs::create_dir_all(&outside_run).expect("outside run dir");
        symlink(&outside_run, rootfs.join("run")).expect("run symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("socket run symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &rootfs.join("run")),
            "expected unsafe run path, got: {error:?}"
        );
        assert!(
            !outside_run.join("denia").exists(),
            "prepare must not create socket directories outside the rootfs"
        );
    }

    #[test]
    fn prepare_rejects_socket_proxy_target_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:proxy-link");
        let denia_dir = rootfs.join(".denia");
        std::fs::create_dir_all(&denia_dir).expect(".denia dir");
        let outside_target = tmp.path().join("outside-proxy");
        symlink(&outside_target, denia_dir.join("socket-proxy")).expect("socket proxy symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("socket proxy target symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &rootfs.join(".denia/socket-proxy")),
            "expected unsafe socket proxy target path, got: {error:?}"
        );
        assert!(
            !outside_target.exists(),
            "prepare must not write socket proxy through a rootfs symlink"
        );
    }

    #[test]
    fn prepare_rejects_missing_socket_proxy_source() {
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
        let plan = runtime.plan(&request).expect("plan");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("missing socket proxy rejected");

        assert!(
            matches!(error, RuntimeError::SocketProxyUnavailable { ref path } if path == &missing_proxy),
            "expected missing socket proxy source, got: {error:?}"
        );
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
                program: "launcher".into(),
                args: Vec::new(),
                env: Vec::new(),
                namespace: NamespaceConfig::new(
                    tmp.path().join("rootfs"),
                    vec!["/bin/true".to_string()],
                )
                .with_uid_map(100000, 65536)
                .with_cgroup_path(cgroup_path.clone()),
                rootfs_path: tmp.path().join("rootfs"),
                socket_path: tmp.path().join("rootfs/run/denia/service.sock"),
                guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
                cgroup_path: cgroup_path.clone(),
                service_dir: tmp.path().join("runtime/test-svc"),
                deployment_dir: tmp.path().join("runtime/test-svc/deployment"),
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
}
