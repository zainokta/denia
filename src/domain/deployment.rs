use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_request_round_trips_and_exposes_service_id() {
        let sid = Uuid::now_v7();
        let req = DeploymentRequest::Upload {
            service_id: sid,
            upload_id: "abc".into(),
            dockerfile_path: "Dockerfile".into(),
            context_path: ".".into(),
        };
        assert_eq!(req.service_id(), sid);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"source\":\"upload\""));
        let back: DeploymentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn redeploy_request_round_trips_and_exposes_service_id() {
        let sid = Uuid::now_v7();
        let req = DeploymentRequest::Redeploy { service_id: sid };
        assert_eq!(req.service_id(), sid);
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            format!(r#"{{"source":"redeploy","service_id":"{sid}"}}"#)
        );
        let back: DeploymentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }
}

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
    Upload {
        service_id: Uuid,
        upload_id: String,
        dockerfile_path: String,
        context_path: String,
    },
    /// Redeploy the service's current promoted artifact without rebuilding from
    /// source. Backs the console "deploy" button for upload-source services
    /// (ADR-039): the coordinator loads the latest promoted deployment's stored
    /// artifact instead of acquiring a new one. Serializes as
    /// `{"source":"redeploy","service_id":"<uuid>"}`.
    Redeploy {
        service_id: Uuid,
    },
}

impl DeploymentRequest {
    pub fn service_id(&self) -> Uuid {
        match self {
            Self::Git { service_id, .. }
            | Self::ExternalImage { service_id, .. }
            | Self::Upload { service_id, .. }
            | Self::Redeploy { service_id } => *service_id,
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
    /// A previously-promoted deployment that a newer deploy has replaced. Set on
    /// the outgoing promoted row when a different deployment is promoted, so the
    /// history shows exactly one live (`Healthy`) deployment per service.
    Inactive,
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
    #[serde(default)]
    pub replica_index: u32,
}

impl RuntimeStartRequest {
    pub fn instance_id(&self) -> RuntimeInstanceId {
        RuntimeInstanceId {
            service_id: self.service_id,
            service_name: self.service_name.clone(),
            replica_index: self.replica_index,
        }
    }
}

/// Identity of a single running replica of a service.
///
/// A service may run multiple replicas (autoscaling). The *identity* of a
/// replica is `(service_id, replica_index)` — `service_name` is carried for
/// display/logging only and is deliberately excluded from equality and hashing,
/// because service names are only unique within a project and would otherwise
/// let two projects' same-named services collide in runtime state (F-3).
#[derive(Debug, Clone)]
pub struct RuntimeInstanceId {
    pub service_id: Uuid,
    pub service_name: String,
    pub replica_index: u32,
}

impl PartialEq for RuntimeInstanceId {
    fn eq(&self, other: &Self) -> bool {
        self.service_id == other.service_id && self.replica_index == other.replica_index
    }
}

impl Eq for RuntimeInstanceId {}

impl std::hash::Hash for RuntimeInstanceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.service_id.hash(state);
        self.replica_index.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub service_id: Uuid,
    pub service_name: String,
    pub deployment_id: Uuid,
    pub state: String,
    pub pid: Option<u32>,
    pub cgroup_path: std::path::PathBuf,
    pub socket_path: std::path::PathBuf,
    #[serde(default)]
    pub replica_index: u32,
}
