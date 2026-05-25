use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

use crate::artifacts::ArtifactKind;
use crate::cgroup_launcher;
use crate::domain::{RuntimeStartRequest, RuntimeStatus};

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
    #[error("setpriv binary not found: {path}")]
    SetprivUnavailable { path: PathBuf },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest json error: {0}")]
    Manifest(#[from] serde_json::Error),
}

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError>;
    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError>;
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
    pub rootfs_path: PathBuf,
    pub socket_path: PathBuf,
    pub guest_socket_path: String,
    pub cgroup_path: PathBuf,
    pub cgroup_ready_path: PathBuf,
    pub service_dir: PathBuf,
    pub deployment_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LinuxRuntime {
    runtime_dir: PathBuf,
    artifact_dir: PathBuf,
    cgroup_root: PathBuf,
    log_dir: PathBuf,
    cgroup_launcher_binary: PathBuf,
    unshare_binary: PathBuf,
    userns_base: u32,
    userns_size: u32,
    setpriv_source: PathBuf,
    children: Arc<Mutex<HashMap<String, TrackedChild>>>,
}

const SETPRIV_TARGET: &str = "/.denia/setpriv";
const SOCKET_PROXY_TARGET: &str = "/.denia/socket-proxy";
const GUEST_SERVICE_SOCKET: &str = "/run/denia/service.sock";
const GUEST_SERVICE_SOCKET_ENV: &str = "DENIA_SERVICE_SOCKET";

#[derive(Debug)]
struct TrackedChild {
    child: tokio::process::Child,
    plan: LinuxRuntimePlan,
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
            cgroup_launcher_binary: std::env::current_exe().unwrap_or_else(|_| "denia".into()),
            unshare_binary: unshare_binary.into(),
            userns_base: 100000,
            userns_size: 65536,
            setpriv_source: PathBuf::from("setpriv"),
            children: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_userns(mut self, base: u32, size: u32) -> Self {
        self.userns_base = base;
        self.userns_size = size;
        self
    }

    pub fn with_setpriv(mut self, path: impl Into<PathBuf>) -> Self {
        self.setpriv_source = path.into();
        self
    }

    pub fn with_log_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_dir = path.into();
        self
    }

    pub fn with_cgroup_launcher(mut self, path: impl Into<PathBuf>) -> Self {
        self.cgroup_launcher_binary = path.into();
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
        let mut env = process.env;
        env.retain(|(key, _)| key != GUEST_SERVICE_SOCKET_ENV);
        env.push((
            GUEST_SERVICE_SOCKET_ENV.to_string(),
            GUEST_SERVICE_SOCKET.to_string(),
        ));

        let service_dir = self.runtime_dir.join(&request.service_name);
        let deployment_dir = service_dir.join(request.deployment_id.to_string());
        let socket_path = rootfs_path.join(GUEST_SERVICE_SOCKET.trim_start_matches('/'));
        let cgroup_path = self
            .cgroup_root
            .join(&request.service_name)
            .join(request.deployment_id.to_string());
        let cgroup_ready_path = deployment_dir.join("cgroup.ready");
        let mut args = vec![
            cgroup_launcher::MODE_ARG.to_string(),
            "--cgroup-procs".to_string(),
            cgroup_path.join("cgroup.procs").display().to_string(),
            "--ready-file".to_string(),
            cgroup_ready_path.display().to_string(),
            "--".to_string(),
            self.unshare_binary.display().to_string(),
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
            SOCKET_PROXY_TARGET.to_string(),
            "--listen".to_string(),
            GUEST_SERVICE_SOCKET.to_string(),
            "--connect".to_string(),
            format!("127.0.0.1:{}", request.internal_port),
            "--".to_string(),
            SETPRIV_TARGET.to_string(),
            "--no-new-privs".to_string(),
            "--bounding-set".to_string(),
            "-all".to_string(),
            "--".to_string(),
        ];
        args.extend(process.argv);

        Ok(LinuxRuntimePlan {
            program: self.cgroup_launcher_binary.clone(),
            args,
            env,
            rootfs_path,
            socket_path,
            guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
            cgroup_path,
            cgroup_ready_path,
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
        validate_namespace_launcher(&plan.args)?;
        self.inject_setpriv(&plan.rootfs_path)?;
        self.inject_socket_proxy(&plan.rootfs_path)?;
        self.prepare_socket_directory(plan)?;
        std::fs::create_dir_all(&plan.deployment_dir)?;
        remove_file_if_exists(&plan.cgroup_ready_path)?;
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

    fn inject_setpriv(&self, rootfs: &Path) -> Result<(), RuntimeError> {
        let setpriv_path = resolve_setpriv(&self.setpriv_source).ok_or_else(|| {
            RuntimeError::SetprivUnavailable {
                path: self.setpriv_source.clone(),
            }
        })?;
        let target_dir = rootfs.join(".denia");
        create_runtime_directory(&target_dir)?;
        let target = target_dir.join("setpriv");
        remove_existing_runtime_file(&target)?;
        std::fs::copy(&setpriv_path, &target)?;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)?;
        Ok(())
    }

    fn inject_socket_proxy(&self, rootfs: &Path) -> Result<(), RuntimeError> {
        let proxy_source = std::env::current_exe()?;
        let target_dir = rootfs.join(".denia");
        create_runtime_directory(&target_dir)?;
        let target = target_dir.join("socket-proxy");
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

fn resolve_setpriv(source: &Path) -> Option<PathBuf> {
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

fn validate_namespace_launcher(args: &[String]) -> Result<(), RuntimeError> {
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        return Ok(());
    };
    let Some(launcher) = args.get(separator + 1) else {
        return Ok(());
    };
    let launcher_path = Path::new(launcher);
    if (launcher_path.is_absolute() || launcher.contains('/')) && !launcher_path.exists() {
        return Err(RuntimeError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("namespace launcher not found: {launcher}"),
        )));
    }
    Ok(())
}

#[async_trait]
impl Runtime for LinuxRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.reap_exited_children()?;
        let plan = self.plan(&request)?;
        self.prepare(&plan, &request)?;
        let status_socket_path = plan.socket_path.clone();
        let log_file = match self.open_log_file(&request.service_name) {
            Ok(file) => file,
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Io(error));
            }
        };
        let stderr_log_file = match log_file.try_clone() {
            Ok(file) => file,
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Io(error));
            }
        };

        let mut command = Command::new(&plan.program);
        command
            .args(&plan.args)
            .env_clear()
            .envs(plan.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_log_file));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = self.cleanup(&plan);
                return Err(RuntimeError::Io(error));
            }
        };
        let pid = child.id();
        if let Err(error) = wait_for_cgroup_ready(&mut child, &plan.cgroup_ready_path).await {
            let _ = child.kill().await;
            let _ = self.cleanup(&plan);
            return Err(error);
        }
        let status_cgroup_path = plan.cgroup_path.clone();
        let tracked_child = TrackedChild { child, plan };
        let replaced_child = match self.children.lock() {
            Ok(mut children) => children.insert(request.service_name.clone(), tracked_child),
            Err(_) => {
                let mut child = tracked_child.child;
                let _ = child.start_kill();
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
                if tracked.child.try_wait()?.is_some() {
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

    fn open_log_file(&self, service_name: &str) -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(&self.log_dir)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir.join(format!("{service_name}.log")))
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

fn remove_file_if_exists(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::remove_file(path) {
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
    if tracked.child.try_wait()?.is_none() {
        tracked.child.kill().await?;
    }
    Ok(())
}

async fn wait_for_cgroup_ready(
    child: &mut tokio::process::Child,
    ready_path: &Path,
) -> Result<(), RuntimeError> {
    for _ in 0..100 {
        if ready_path.exists() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(RuntimeError::Io(std::io::Error::other(format!(
                "cgroup launcher exited before cgroup placement: {status}"
            ))));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(RuntimeError::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "timed out waiting for cgroup launcher placement",
    )))
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
            deployment_id: uuid::Uuid::now_v7(),
            artifact,
            internal_port: 8080,
            socket_path: runtime_dir.join(service_name).join("current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
        }
    }

    #[test]
    fn plan_includes_user_namespace_and_setpriv_wrapper() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let runtime =
            LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir.clone(), cgroup_dir)
                .with_userns(200000, 10000)
                .with_setpriv("/usr/local/bin/setpriv");

        let (artifact, _, _) = write_process_bundle(&artifact_dir, "sha256:abc");
        let request = runtime_request(&runtime_dir, artifact, "test-svc");

        let plan = runtime.plan(&request).expect("plan");

        let args = &plan.args;

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

        assert_eq!(plan.program, runtime.cgroup_launcher_binary);
        assert_eq!(args[0], cgroup_launcher::MODE_ARG);
        let expected_cgroup_procs = plan.cgroup_path.join("cgroup.procs").display().to_string();
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--cgroup-procs" && window[1] == expected_cgroup_procs),
            "cgroup launcher should receive cgroup.procs path"
        );
        let sep_pos = args
            .iter()
            .position(|a| a == "/.denia/socket-proxy")
            .expect("socket proxy present");
        assert!(
            args[sep_pos - 1] == "--",
            "socket proxy follows unshare separator"
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
            args[sep_pos + 6] == "/.denia/setpriv",
            "setpriv wrapper after proxy separator"
        );
        assert!(
            args[sep_pos + 7] == "--no-new-privs",
            "--no-new-privs after setpriv"
        );
        assert!(
            args[sep_pos + 8] == "--bounding-set",
            "--bounding-set after --no-new-privs"
        );
        assert!(args[sep_pos + 9] == "-all", "-all after --bounding-set");
        assert!(
            args[sep_pos + 10] == "--",
            "setpriv child separator present"
        );
        assert!(
            args[sep_pos + 11] == "/bin/echo",
            "workload argv follows second --"
        );
        assert!(
            args[sep_pos + 12] == "hello",
            "workload arg follows argv[0]"
        );
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
    fn prepare_rejects_setpriv_target_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let setpriv_source = tmp.path().join("setpriv-source");
        std::fs::write(&setpriv_source, b"setpriv").expect("setpriv source");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:setpriv-link");
        let denia_dir = rootfs.join(".denia");
        std::fs::create_dir_all(&denia_dir).expect(".denia dir");
        let outside_target = tmp.path().join("outside-setpriv");
        symlink(&outside_target, denia_dir.join("setpriv")).expect("setpriv symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir)
            .with_setpriv(&setpriv_source);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("setpriv target symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &rootfs.join(".denia/setpriv")),
            "expected unsafe setpriv target path, got: {error:?}"
        );
        assert!(
            !outside_target.exists(),
            "prepare must not write through a rootfs symlink"
        );
    }

    #[test]
    fn prepare_rejects_denia_directory_symlink() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let runtime_dir = tmp.path().join("runtime");
        let artifact_dir = tmp.path().join("artifacts");
        let cgroup_dir = tmp.path().join("cgroup");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::create_dir_all(&artifact_dir).expect("artifact dir");

        let setpriv_source = tmp.path().join("setpriv-source");
        std::fs::write(&setpriv_source, b"setpriv").expect("setpriv source");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:denia-link");
        let outside_dir = tmp.path().join("outside-denia");
        std::fs::create_dir_all(&outside_dir).expect("outside dir");
        symlink(&outside_dir, rootfs.join(".denia")).expect(".denia symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir)
            .with_setpriv(&setpriv_source);
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
            !outside_dir.join("setpriv").exists(),
            "prepare must not copy setpriv outside the rootfs"
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

        let setpriv_source = tmp.path().join("setpriv-source");
        std::fs::write(&setpriv_source, b"setpriv").expect("setpriv source");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:run-link");
        let outside_run = tmp.path().join("outside-run");
        std::fs::create_dir_all(&outside_run).expect("outside run dir");
        symlink(&outside_run, rootfs.join("run")).expect("run symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir)
            .with_setpriv(&setpriv_source);
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

        let setpriv_source = tmp.path().join("setpriv-source");
        std::fs::write(&setpriv_source, b"setpriv").expect("setpriv source");

        let (artifact, _bundle_dir, rootfs) =
            write_process_bundle(&artifact_dir, "sha256:proxy-link");
        let denia_dir = rootfs.join(".denia");
        std::fs::create_dir_all(&denia_dir).expect(".denia dir");
        let outside_target = tmp.path().join("outside-proxy");
        symlink(&outside_target, denia_dir.join("socket-proxy")).expect("socket proxy symlink");

        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir)
            .with_setpriv(&setpriv_source);
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

    #[tokio::test]
    async fn terminate_tracked_child_uses_cgroup_kill_when_available() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cgroup_path = tmp.path().join("cgroup");
        std::fs::create_dir_all(&cgroup_path).expect("cgroup dir");
        std::fs::write(cgroup_path.join("cgroup.kill"), "").expect("cgroup.kill");
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep child");
        let mut tracked = TrackedChild {
            child,
            plan: LinuxRuntimePlan {
                program: "launcher".into(),
                args: Vec::new(),
                env: Vec::new(),
                rootfs_path: tmp.path().join("rootfs"),
                socket_path: tmp.path().join("rootfs/run/denia/service.sock"),
                guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
                cgroup_path: cgroup_path.clone(),
                cgroup_ready_path: tmp.path().join("runtime/cgroup.ready"),
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
            tracked.child.try_wait().expect("child status").is_some(),
            "child should be reaped after termination"
        );
    }
}
