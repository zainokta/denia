pub mod acquirer;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    OciImage,
    RootfsBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactSource {
    BuildKit {
        repo_url: String,
        git_ref: String,
        dockerfile_path: String,
        context_path: String,
    },
    ExternalRegistry {
        image: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub id: Uuid,
    pub digest: String,
    pub kind: ArtifactKind,
    pub source: ArtifactSource,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactError {
    #[error("artifact digest cannot be empty")]
    EmptyDigest,
}

impl ArtifactRecord {
    pub fn new(
        digest: impl Into<String>,
        kind: ArtifactKind,
        source: ArtifactSource,
    ) -> Result<Self, ArtifactError> {
        let digest = digest.into();
        if digest.trim().is_empty() {
            return Err(ArtifactError::EmptyDigest);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            digest,
            kind,
            source,
            created_at: Utc::now(),
        })
    }
}
