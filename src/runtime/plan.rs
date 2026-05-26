use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    pub guest_socket_path: String,
    pub cgroup_path: PathBuf,
    pub service_dir: PathBuf,
    pub deployment_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct TrackedChild {
    pub(crate) process: TrackedProcess,
    pub(crate) plan: LinuxRuntimePlan,
}

#[derive(Debug)]
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
