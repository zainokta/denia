use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use crate::artifacts::ArtifactKind;
use crate::domain::{
    JobOutcome, JobRunRequest, RuntimeInstanceId, RuntimeStartRequest, RuntimeStatus,
};
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
    validate_process_spec, validate_resource_limits, validate_service_name,
};
use crate::syscall::chown;
use crate::syscall::ns::{NamespaceConfig, OverlaySpec, RoBind, spawn_namespaced_process};
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
}

pub(crate) const SOCKET_PROXY_TARGET: &str = "/.denia/socket-proxy";
pub(crate) const WORKLOAD_LAUNCHER_TARGET: &str = "/.denia/workload-launcher";
pub(crate) const GUEST_SERVICE_SOCKET: &str = "/run/denia/service.sock";
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
        let replica_dir = deployment_dir.join(request.replica_index.to_string());
        let upper = replica_dir.join("upper");
        let work = replica_dir.join("work");
        let merged = replica_dir.join("merged");
        // The socket-proxy creates the guest socket at GUEST_SERVICE_SOCKET, which
        // lives inside the overlay. Overlay writes land in `upper` (host-visible),
        // whereas `merged` is private to the child's mount namespace. The host-side
        // socket path used by readiness waits, status, and the bridge must therefore
        // be the upper-side path, not rootfs/ nor merged/.
        let socket_path = upper.join(GUEST_SERVICE_SOCKET.trim_start_matches('/'));
        let cgroup_path = self
            .cgroup_root
            .join(request.service_id.to_string())
            .join(request.deployment_id.to_string())
            .join(request.replica_index.to_string());
        let mut child_argv = vec![
            SOCKET_PROXY_TARGET.to_string(),
            "--listen".to_string(),
            GUEST_SERVICE_SOCKET.to_string(),
            "--connect".to_string(),
            format!("127.0.0.1:{}", request.internal_port),
            "--".to_string(),
        ];
        child_argv.extend(process.argv);
        let overlay = OverlaySpec {
            lower: rootfs_path.clone(),
            upper: upper.clone(),
            work: work.clone(),
            merged: merged.clone(),
        };
        let socket_proxy_src = resolve_host_binary(&self.socket_proxy_source).ok_or_else(|| {
            RuntimeError::SocketProxyUnavailable {
                path: self.socket_proxy_source.clone(),
            }
        })?;
        let ro_bind = RoBind {
            src: socket_proxy_src,
            dest: PathBuf::from(SOCKET_PROXY_TARGET),
        };
        // The overlay's `merged` mountpoint becomes the new root (8a pivots into it
        // when an overlay is set), so the namespace root must be `merged`.
        let namespace = NamespaceConfig::new(merged.clone(), child_argv.clone())
            .with_overlay(overlay)
            .with_ro_bind(ro_bind)
            .with_uid_map(self.userns_base, self.userns_size)
            .with_cgroup_path(cgroup_path.clone())
            .with_workdir(process.workdir.clone())
            .with_env(env.clone())
            .with_deferred_hardening();

        Ok(LinuxRuntimePlan {
            namespace,
            rootfs_path,
            socket_path,
            guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
            cgroup_path,
            deployment_id: request.deployment_id,
            service_dir,
            deployment_dir,
            replica_dir,
            upper,
            work,
            merged,
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
        create_dir_all("create replica upper directory", &plan.upper)?;
        create_dir_all("create replica work directory", &plan.work)?;
        create_dir_all("create replica merged directory", &plan.merged)?;
        self.prepare_socket_directory(plan)?;
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
        // The upper layer is ephemeral: wiping the replica dir removes
        // upper/work/merged so a relaunch starts from a clean writable layer.
        remove_dir_if_exists(&plan.replica_dir)?;
        Ok(())
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
        std::fs::copy(&proxy_source, &target)
            .map_err(path_io("copy runtime helper into rootfs", &target))?;
        let mut perms = std::fs::metadata(&target)
            .map_err(path_io("stat injected runtime helper", &target))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)
            .map_err(path_io("chmod injected runtime helper", &target))?;
        Ok(())
    }

    fn prepare_socket_directory(&self, plan: &LinuxRuntimePlan) -> Result<(), RuntimeError> {
        // The socket lives in the writable upper layer (host-visible) so the
        // host-side socket path resolves once the guest socket-proxy binds it.
        let run_dir = plan.upper.join("run");
        let socket_dir = plan.upper.join("run/denia");
        create_runtime_directory(&run_dir)?;
        create_runtime_directory(&socket_dir)?;
        self.chown_socket_directory(&run_dir)?;
        self.chown_socket_directory(&socket_dir)?;
        remove_existing_runtime_file(&plan.socket_path)?;
        Ok(())
    }

    fn chown_socket_directory(&self, path: &Path) -> Result<(), RuntimeError> {
        if !rustix::process::geteuid().is_root() {
            return Ok(());
        }
        chown::recursive_lchown(path, self.userns_base, self.userns_base)?;
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
        let mut process = TrackedProcess::NativePid(pid.expect("native pid"));
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
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid,
            cgroup_path: status_cgroup_path,
            socket_path: status_socket_path,
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
        self.inject_workload_launcher(&rootfs_path)?;
        std::fs::create_dir_all(&run_dir)?;
        prepare_cgroup_directory(&self.cgroup_root, &cgroup_path, CGROUP_CONTROLLERS)?;
        std::fs::write(cgroup_path.join("cpu.max"), cpu_max(request.cpu_millis))?;
        std::fs::write(
            cgroup_path.join("memory.max"),
            format!("{}\n", request.memory_bytes),
        )?;
        if let Some(swap) = request.memory_swap_max {
            let _ = std::fs::write(cgroup_path.join("memory.swap.max"), format!("{}\n", swap));
        }
        if let Some(pids) = request.pids_max {
            let _ = std::fs::write(cgroup_path.join("pids.max"), format!("{}\n", pids));
        }
        if let Some(weight) = request.io_weight {
            let _ = std::fs::write(cgroup_path.join("io.weight"), format!("{}\n", weight));
        }

        let cleanup = || {
            let _ = std::fs::write(cgroup_path.join("cgroup.kill"), "1\n");
            let _ = remove_cgroup_dir_if_exists(&cgroup_path);
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
                    service_name: instance.service_name.clone(),
                    deployment_id: tracked.plan.deployment_id,
                    state: "running".to_string(),
                    pid: Some(pid),
                    cgroup_path: tracked.plan.cgroup_path.clone(),
                    socket_path: tracked.plan.socket_path.clone(),
                    replica_index: instance.replica_index,
                }
            })
            .collect();
        Ok(statuses)
    }
}

impl LinuxRuntime {
    fn reap_exited_children(&self) -> Result<(), RuntimeError> {
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

    fn service_log_path(&self, service_name: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.log_dir)?;
        let path = self.log_dir.join(format!("{service_name}.log"));
        OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CGROUP_CONTROLLERS, GUEST_SERVICE_SOCKET, GUEST_SERVICE_SOCKET_ENV, LinuxRuntime,
        LinuxRuntimePlan, LinuxRuntimeProcessSpec, TrackedChild, TrackedProcess,
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

        // Host-side socket path is the upper-side path, not rootfs/ nor merged/.
        assert!(
            plan0.socket_path.starts_with(&plan0.upper),
            "socket path {} must be under upper {}",
            plan0.socket_path.display(),
            plan0.upper.display()
        );
        assert!(
            plan0.socket_path.ends_with("upper/run/denia/service.sock"),
            "unexpected socket path: {}",
            plan0.socket_path.display()
        );
        assert!(
            !plan0.socket_path.starts_with(&plan0.rootfs_path),
            "socket path must not be inside the read-only rootfs"
        );
        assert!(
            !plan0.socket_path.starts_with(&plan0.merged),
            "socket path must not be the namespace-private merged mount"
        );
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
                GUEST_SERVICE_SOCKET.to_string()
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
            plan.upper.join("run/denia").is_dir(),
            "prepare must create the socket directory in the per-replica upper layer"
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

        let (artifact, _bundle_dir, _rootfs) =
            write_process_bundle(&artifact_dir, "sha256:run-link");
        let runtime = LinuxRuntime::new_with_paths(runtime_dir.clone(), artifact_dir, cgroup_dir);
        let request = runtime_request(&runtime_dir, artifact, "test-svc");
        let plan = runtime.plan(&request).expect("plan");
        // Pre-plant a symlink at the upper-side run dir before prepare runs.
        std::fs::create_dir_all(&plan.upper).expect("upper dir");
        let outside_run = tmp.path().join("outside-run");
        std::fs::create_dir_all(&outside_run).expect("outside run dir");
        symlink(&outside_run, plan.upper.join("run")).expect("run symlink");

        let error = runtime
            .prepare(&plan, &request)
            .expect_err("socket run symlink rejected");

        assert!(
            matches!(error, RuntimeError::UnsafeRuntimePath { ref path } if path == &plan.upper.join("run")),
            "expected unsafe run path, got: {error:?}"
        );
        assert!(
            !outside_run.join("denia").exists(),
            "prepare must not create socket directories outside the upper layer"
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
        std::fs::create_dir_all(plan.upper.join("run/denia")).expect("socket dir");
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
        std::fs::create_dir_all(plan.upper.join("run/denia")).expect("socket dir");
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
                guest_socket_path: GUEST_SERVICE_SOCKET.to_string(),
                cgroup_path: cgroup_path.clone(),
                deployment_id: uuid::Uuid::now_v7(),
                service_dir: tmp.path().join("runtime/test-svc"),
                deployment_dir: tmp.path().join("runtime/test-svc/deployment"),
                replica_dir: tmp.path().join("runtime/test-svc/deployment/0"),
                upper: tmp.path().join("runtime/test-svc/deployment/0/upper"),
                work: tmp.path().join("runtime/test-svc/deployment/0/work"),
                merged: tmp.path().join("runtime/test-svc/deployment/0/merged"),
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
}
