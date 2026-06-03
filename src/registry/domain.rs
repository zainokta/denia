use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedRepository {
    pub id: Uuid,
    pub project_id: Uuid,
    pub service_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedManifest {
    pub repository_id: Uuid,
    pub digest: String,
    pub media_type: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedTag {
    pub repository_id: Uuid,
    pub tag: String,
    pub manifest_digest: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedUpload {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub path: std::path::PathBuf,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
