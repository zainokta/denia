//! Job + job-run scheduling repository trait.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::{Job, JobRun, JobRunStatus};
use crate::repo::error::RepoError;

pub trait JobRepo: Send + Sync + 'static {
    fn put_job(&self, job: Job) -> Result<Job, RepoError>;
    fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, RepoError>;
    fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, RepoError>;
    fn delete_job(&self, job_id: Uuid) -> Result<(), RepoError>;
    fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, RepoError>;
    fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, RepoError>;
    fn update_job_run(
        &self,
        run_id: Uuid,
        status: JobRunStatus,
        exit_code: Option<i32>,
    ) -> Result<(), RepoError>;
    fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, RepoError>;
    fn fail_orphan_runs(&self) -> Result<usize, RepoError>;
    fn claim_due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<Job>, RepoError>;
    fn set_job_next_run(
        &self,
        job_id: Uuid,
        next_run_at: Option<DateTime<Utc>>,
        last_enqueued_at: Option<DateTime<Utc>>,
    ) -> Result<(), RepoError>;
}
