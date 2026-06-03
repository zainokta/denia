use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runtime::error::RuntimeError;
use crate::syscall::ns::NamespaceConfig;
use crate::syscall::signal::{self, ProcessStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxRuntimeProcessSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxRuntimePlan {
    pub namespace: NamespaceConfig,
    pub rootfs_path: PathBuf,
    pub socket_path: PathBuf,
    /// Short host-side alias (symlink) to `socket_path`, kept under the
    /// `sockaddr_un` 108-byte limit so the ingress and clients can `connect()`.
    /// The real `socket_path` (~127B, deep under the per-replica upper) exceeds
    /// that limit and cannot be connected directly.
    pub socket_connect_path: PathBuf,
    pub guest_socket_path: String,
    pub cgroup_path: PathBuf,
    pub deployment_id: Uuid,
    pub service_dir: PathBuf,
    pub deployment_dir: PathBuf,
    /// Per-replica overlay root (`{service_id}/{deployment_id}/{replica_index}`).
    /// Holds the writable `upper`, overlay `work`, and `merged` mountpoint.
    pub replica_dir: PathBuf,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub merged: PathBuf,
    /// Base artifact directory (e.g., `/var/lib/denia/artifacts`).
    /// The child process needs traverse permission to access the lowerdir.
    pub artifact_dir: PathBuf,
    /// Base runtime directory (e.g., `/var/lib/denia/runtime`).
    /// The child process needs traverse permission to access overlay directories.
    pub runtime_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct TrackedChild {
    pub(crate) process: TrackedProcess,
    pub(crate) plan: LinuxRuntimePlan,
}

impl TrackedChild {
    /// Snapshot of the tracked child for read-only use (console attach). The
    /// original entry stays in the runtime's `children` map; the console only
    /// needs the target pid, cgroup, and rootfs/namespace plan.
    pub(crate) fn clone_for_console(&self) -> Self {
        Self {
            process: self.process.clone(),
            plan: self.plan.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TrackedProcess {
    NativePid(u32),
}

impl TrackedProcess {
    pub(crate) fn try_wait(&mut self) -> Result<bool, RuntimeError> {
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
