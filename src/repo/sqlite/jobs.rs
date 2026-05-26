//! `SqliteStore` impl block for job + job-run aggregate methods.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Job, JobRun, JobRunStatus};
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn put_job(&self, job: Job) -> Result<Job, StateError> {
        let connection = self.connection()?;
        let json = serde_json::to_string(&job)?;
        connection.execute(
            "INSERT INTO jobs (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
            params![
                job.id.to_string(),
                job.project_id.to_string(),
                job.name,
                &json
            ],
        )?;
        Ok(job)
    }

    pub fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, StateError> {
        let connection = self.connection()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT config_json FROM jobs WHERE id = ?1",
                params![job_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(Into::into)
    }

    pub fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("SELECT config_json FROM jobs WHERE project_id = ?1 ORDER BY name")?;
        let rows = stmt.query_map(params![project_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(serde_json::from_str(&row?)?);
        }
        Ok(jobs)
    }

    pub fn delete_job(&self, job_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM job_runs WHERE job_id = ?1",
            params![job_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM jobs WHERE id = ?1",
            params![job_id.to_string()],
        )?;
        Ok(())
    }

    pub fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, StateError> {
        let run = JobRun {
            id: Uuid::now_v7(),
            job_id,
            status: JobRunStatus::Pending,
            attempt: 1,
            exit_code: None,
            started_at: None,
            finished_at: None,
            created_at: Utc::now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO job_runs (id, job_id, status, attempt, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run.id.to_string(),
                job_id.to_string(),
                serde_json::to_string(&run.status)?,
                run.attempt,
                run.created_at.to_rfc3339(),
            ],
        )?;
        Ok(run)
    }

    pub fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, job_id, status, attempt, exit_code, started_at, finished_at, created_at
             FROM job_runs WHERE job_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![job_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, Option<i32>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (id, jid, status_str, attempt, exit_code, started_at, finished_at, created_at) =
                row?;
            runs.push(JobRun {
                id: Uuid::parse_str(&id)?,
                job_id: Uuid::parse_str(&jid)?,
                status: serde_json::from_str(&status_str)?,
                attempt,
                exit_code,
                started_at: started_at.and_then(|s| s.parse().ok()),
                finished_at: finished_at.and_then(|s| s.parse().ok()),
                created_at: created_at.parse()?,
            });
        }
        Ok(runs)
    }

    pub fn update_job_run(
        &self,
        run_id: Uuid,
        status: JobRunStatus,
        exit_code: Option<i32>,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE job_runs SET status = ?1, exit_code = ?2, finished_at = ?3 WHERE id = ?4",
            params![
                serde_json::to_string(&status)?,
                exit_code,
                Utc::now().to_rfc3339(),
                run_id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, StateError> {
        let runs = self.list_job_runs(job_id)?;
        let pending = serde_json::to_string(&JobRunStatus::Pending)?;
        let running = serde_json::to_string(&JobRunStatus::Running)?;
        Ok(runs.into_iter().find(|r| {
            let s = serde_json::to_string(&r.status).unwrap_or_default();
            s == pending || s == running
        }))
    }

    pub fn fail_orphan_runs(&self) -> Result<usize, StateError> {
        let connection = self.connection()?;
        let pending = serde_json::to_string(&JobRunStatus::Pending)?;
        let running = serde_json::to_string(&JobRunStatus::Running)?;
        let failed = serde_json::to_string(&JobRunStatus::Failed)?;
        let updated = connection.execute(
            "UPDATE job_runs SET status = ?1, finished_at = ?2 WHERE status = ?3 OR status = ?4",
            params![&failed, Utc::now().to_rfc3339(), &pending, &running],
        )?;
        Ok(updated)
    }

    pub fn claim_due_jobs(&self, now: chrono::DateTime<Utc>) -> Result<Vec<Job>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare("SELECT config_json FROM jobs")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut due = Vec::new();
        for row in rows {
            let json = row?;
            let job: Job = serde_json::from_str(&json)?;
            if job.next_run_at.map(|next| next <= now).unwrap_or(false) {
                due.push(job);
            }
        }
        Ok(due)
    }

    pub fn set_job_next_run(
        &self,
        job_id: Uuid,
        next_run_at: Option<chrono::DateTime<Utc>>,
        last_enqueued_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<(), StateError> {
        let mut job = self
            .get_job(job_id)?
            .ok_or(StateError::InvalidCredentials)?;
        job.next_run_at = next_run_at;
        job.last_enqueued_at = last_enqueued_at;
        let connection = self.connection()?;
        connection.execute(
            "UPDATE jobs SET config_json = ?1 WHERE id = ?2",
            params![serde_json::to_string(&job)?, job_id.to_string()],
        )?;
        Ok(())
    }
}
