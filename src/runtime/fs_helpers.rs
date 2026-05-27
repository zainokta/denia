use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

use tokio::time::{Duration, Instant, sleep};

use crate::runtime::error::RuntimeError;
use crate::runtime::plan::{TrackedChild, TrackedProcess};
use crate::syscall::signal::{self, ProcessStatus};

pub(crate) const SERVICE_SOCKET_READY_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SERVICE_SOCKET_READY_POLL: Duration = Duration::from_millis(50);

pub(crate) fn resolve_host_binary(source: &Path) -> Option<PathBuf> {
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

pub(crate) fn path_io(
    action: &'static str,
    path: impl Into<PathBuf>,
) -> impl FnOnce(std::io::Error) -> RuntimeError {
    let path = path.into();
    move |source| RuntimeError::PathIo {
        action,
        path,
        source,
    }
}

pub(crate) fn create_dir_all(action: &'static str, path: &Path) -> Result<(), RuntimeError> {
    std::fs::create_dir_all(path).map_err(path_io(action, path))
}

pub(crate) fn prepare_cgroup_directory(
    root: &Path,
    path: &Path,
    controllers: &[&str],
) -> Result<(), RuntimeError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| RuntimeError::UnsafeRuntimePath {
            path: path.to_path_buf(),
        })?;
    let root_existed = root.exists();
    if !root_existed && let Some(parent) = root.parent() {
        enable_cgroup_controllers(parent, controllers)?;
    }
    create_dir_all("create cgroup root directory", root)?;
    if !cgroup_has_requested_controllers(root, controllers)?
        && let Some(parent) = root.parent()
    {
        enable_cgroup_controllers(parent, controllers)?;
    }
    enable_cgroup_controllers(root, controllers)?;

    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(RuntimeError::UnsafeRuntimePath {
                path: path.to_path_buf(),
            });
        };
        current.push(part);
        create_dir_all("create cgroup directory", &current)?;
        if current != path {
            enable_cgroup_controllers(&current, controllers)?;
        }
    }

    Ok(())
}

pub(crate) fn enable_cgroup_controllers(
    path: &Path,
    controllers: &[&str],
) -> Result<(), RuntimeError> {
    let controllers_path = path.join("cgroup.controllers");
    let subtree_control_path = path.join("cgroup.subtree_control");
    if !controllers_path.exists() || !subtree_control_path.exists() {
        return Ok(());
    }

    let available = std::fs::read_to_string(&controllers_path)
        .map_err(path_io("read cgroup controllers", &controllers_path))?;
    let requested = controllers
        .iter()
        .filter(|controller| {
            available
                .split_whitespace()
                .any(|available| available == **controller)
        })
        .map(|controller| format!("+{controller}"))
        .collect::<Vec<_>>();
    if requested.is_empty() {
        return Ok(());
    }

    std::fs::write(&subtree_control_path, format!("{}\n", requested.join(" ")))
        .map_err(path_io("enable cgroup controllers", &subtree_control_path))
}

pub(crate) fn cgroup_has_requested_controllers(
    path: &Path,
    controllers: &[&str],
) -> Result<bool, RuntimeError> {
    let controllers_path = path.join("cgroup.controllers");
    if !controllers_path.exists() {
        return Ok(true);
    }
    let available = std::fs::read_to_string(&controllers_path)
        .map_err(path_io("read cgroup controllers", &controllers_path))?;
    Ok(controllers.iter().all(|controller| {
        available
            .split_whitespace()
            .any(|available| available == *controller)
    }))
}

pub(crate) fn safe_artifact_name(digest: &str) -> String {
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

pub(crate) fn cpu_max(cpu_millis: u32) -> String {
    format!("{} 100000\n", u64::from(cpu_millis) * 100)
}

pub(crate) fn remove_dir_if_exists(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::Io(error)),
    }
}

pub(crate) fn remove_cgroup_dir_if_exists(path: &Path) -> Result<(), RuntimeError> {
    match remove_cgroup_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::Io(error)),
    }
}

pub(crate) fn remove_cgroup_dir(path: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_cgroup_dir(&entry.path())?;
        }
    }
    std::fs::remove_dir(path)
}

pub(crate) async fn wait_for_service_socket(
    path: &Path,
    process: &mut TrackedProcess,
) -> Result<(), RuntimeError> {
    let deadline = Instant::now() + SERVICE_SOCKET_READY_TIMEOUT;
    loop {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RuntimeError::Io(error)),
        }
        if process.try_wait()? {
            return Err(RuntimeError::ServiceSocketUnavailable {
                path: path.to_path_buf(),
            });
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::ServiceSocketUnavailable {
                path: path.to_path_buf(),
            });
        }
        sleep(SERVICE_SOCKET_READY_POLL).await;
    }
}

/// Grace period a workload gets to exit after SIGTERM before it is force-killed.
pub(crate) const TERMINATION_GRACE: Duration = Duration::from_secs(10);
const TERMINATION_POLL: Duration = Duration::from_millis(100);

pub(crate) async fn terminate_tracked_child(
    tracked: &mut TrackedChild,
) -> Result<(), RuntimeError> {
    // Graceful shutdown first (SIGTERM + grace, then SIGKILL on the main pid).
    terminate_tracked_process(&mut tracked.process).await?;
    // Backstop: kill any stragglers left in the cgroup.
    match std::fs::write(tracked.plan.cgroup_path.join("cgroup.kill"), "1\n") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Io(error)),
    }
    Ok(())
}

pub(crate) async fn terminate_tracked_process(
    process: &mut TrackedProcess,
) -> Result<(), RuntimeError> {
    match process {
        TrackedProcess::NativePid(pid) => {
            if *pid == 0 {
                return Ok(());
            }
            let raw_pid = *pid;
            if signal::try_wait(raw_pid)? == ProcessStatus::Running {
                // Ask the workload to shut down cleanly first.
                let _ = signal::kill(raw_pid, rustix::process::Signal::TERM);
                let deadline = Instant::now() + TERMINATION_GRACE;
                loop {
                    if signal::try_wait(raw_pid)? != ProcessStatus::Running {
                        *pid = 0;
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    sleep(TERMINATION_POLL).await;
                }
                // Grace expired; force kill and reap.
                signal::kill(raw_pid, rustix::process::Signal::KILL)?;
                let _ = tokio::task::spawn_blocking(move || signal::wait(raw_pid)).await??;
            }
            *pid = 0;
        }
    }
    Ok(())
}

pub(crate) fn validate_runtime_directory(path: &Path) -> Result<(), RuntimeError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(path_io("stat runtime directory", path))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RuntimeError::UnsafeRuntimePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub(crate) fn create_runtime_directory(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RuntimeError::UnsafeRuntimePath {
                    path: path.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_dir_all("create runtime directory", path)?;
            validate_runtime_directory(path)?;
        }
        Err(error) => return Err(path_io("stat runtime directory", path)(error)),
    }
    Ok(())
}

pub(crate) fn remove_existing_runtime_file(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || metadata.is_dir() {
                return Err(RuntimeError::UnsafeRuntimePath {
                    path: path.to_path_buf(),
                });
            }
            std::fs::remove_file(path).map_err(path_io("remove existing runtime file", path))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(path_io("stat existing runtime file", path)(error)),
    }
    Ok(())
}

pub(crate) fn exit_code_from_process_status(status: ProcessStatus) -> i32 {
    match status {
        ProcessStatus::Running => -1,
        ProcessStatus::Exited(code) => code,
        ProcessStatus::Signaled(signal) => 128 + signal,
    }
}
