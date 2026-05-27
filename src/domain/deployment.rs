use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum DeploymentRequest {
    Git {
        service_id: Uuid,
        repo_url: String,
        git_ref: String,
    },
    ExternalImage {
        service_id: Uuid,
        image: String,
    },
}

impl DeploymentRequest {
    pub fn service_id(&self) -> Uuid {
        match self {
            Self::Git { service_id, .. } | Self::ExternalImage { service_id, .. } => *service_id,
        }
    }

    pub fn external_image(service_id: Uuid, image: impl Into<String>) -> Self {
        Self::ExternalImage {
            service_id,
            image: image.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentStatus {
    Pending,
    Building,
    Starting,
    Healthy,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    pub id: Uuid,
    pub service_id: Uuid,
    pub request: DeploymentRequest,
    pub status: DeploymentStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStartRequest {
    pub service_name: String,
    pub service_id: Uuid,
    pub deployment_id: Uuid,
    pub artifact: crate::artifacts::ArtifactRecord,
    pub internal_port: u16,
    pub socket_path: std::path::PathBuf,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub pids_max: Option<u64>,
    #[serde(default)]
    pub memory_swap_max: Option<u64>,
    #[serde(default)]
    pub io_weight: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub service_name: String,
    pub deployment_id: Uuid,
    pub state: String,
    pub pid: Option<u32>,
    pub cgroup_path: std::path::PathBuf,
    pub socket_path: std::path::PathBuf,
}
