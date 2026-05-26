use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::service::ServiceSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobRunStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub source: ServiceSource,
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub schedule: Option<String>,
    pub max_retries: u32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_enqueued_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Job {
    pub fn new(
        project_id: Uuid,
        name: impl Into<String>,
        source: ServiceSource,
        schedule: Option<String>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        if let Some(ref s) = schedule {
            let _ = s
                .parse::<cron::Schedule>()
                .map_err(|_| DomainError::InvalidSchedule)?;
        }
        Ok(Self {
            id: Uuid::now_v7(),
            project_id,
            name,
            source,
            command: None,
            env: Vec::new(),
            schedule,
            max_retries: 0,
            next_run_at: None,
            last_enqueued_at: None,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRun {
    pub id: Uuid,
    pub job_id: Uuid,
    pub status: JobRunStatus,
    pub attempt: u32,
    pub exit_code: Option<i32>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunRequest {
    pub job_id: Uuid,
    pub run_id: Uuid,
    pub artifact: crate::artifacts::ArtifactRecord,
    pub command: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub exit_code: i32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}
