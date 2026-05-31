//! Job + job-run aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteJobRepo` delegate.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Job, JobRun, JobRunStatus};
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

pub(super) fn put_job_q(conn: &Connection, job: &Job) -> Result<(), RepoError> {
    let json = serde_json::to_string(job)?;
    conn.execute(
        "INSERT INTO jobs (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            job.id.to_string(),
            job.project_id.to_string(),
            job.name,
            &json
        ],
    )?;
    Ok(())
}

pub(super) fn get_job_q(conn: &Connection, job_id: Uuid) -> Result<Option<Job>, RepoError> {
    let value: Option<(String, String, String)> = conn
        .query_row(
            "SELECT id, project_id, config_json FROM jobs WHERE id = ?1",
            params![job_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    value
        .map(|(id, project_id, json)| parse_job_row(&id, &project_id, &json))
        .transpose()
}

pub(super) fn list_jobs_q(conn: &Connection, project_id: Uuid) -> Result<Vec<Job>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, config_json FROM jobs WHERE project_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![project_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut jobs = Vec::new();
    for row in rows {
        let (id, project_id, json) = row?;
        jobs.push(parse_job_row(&id, &project_id, &json)?);
    }
    Ok(jobs)
}

pub(super) fn delete_job_q(conn: &Connection, job_id: Uuid) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM job_runs WHERE job_id = ?1",
        params![job_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM jobs WHERE id = ?1",
        params![job_id.to_string()],
    )?;
    Ok(())
}

pub(super) fn create_job_run_q(conn: &Connection, job_id: Uuid) -> Result<JobRun, RepoError> {
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
    conn.execute(
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

pub(super) fn list_job_runs_q(conn: &Connection, job_id: Uuid) -> Result<Vec<JobRun>, RepoError> {
    let mut stmt = conn.prepare(
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
        let (id, jid, status_str, attempt, exit_code, started_at, finished_at, created_at) = row?;
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

pub(super) fn update_job_run_q(
    conn: &Connection,
    run_id: Uuid,
    status: JobRunStatus,
    exit_code: Option<i32>,
) -> Result<(), RepoError> {
    conn.execute(
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

pub(super) fn active_run_q(conn: &Connection, job_id: Uuid) -> Result<Option<JobRun>, RepoError> {
    let runs = list_job_runs_q(conn, job_id)?;
    let pending = serde_json::to_string(&JobRunStatus::Pending)?;
    let running = serde_json::to_string(&JobRunStatus::Running)?;
    Ok(runs.into_iter().find(|r| {
        let s = serde_json::to_string(&r.status).unwrap_or_default();
        s == pending || s == running
    }))
}

pub(super) fn fail_orphan_runs_q(conn: &Connection) -> Result<usize, RepoError> {
    let pending = serde_json::to_string(&JobRunStatus::Pending)?;
    let running = serde_json::to_string(&JobRunStatus::Running)?;
    let failed = serde_json::to_string(&JobRunStatus::Failed)?;
    let updated = conn.execute(
        "UPDATE job_runs SET status = ?1, finished_at = ?2 WHERE status = ?3 OR status = ?4",
        params![&failed, Utc::now().to_rfc3339(), &pending, &running],
    )?;
    Ok(updated)
}

pub(super) fn claim_due_jobs_q(
    conn: &Connection,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<Job>, RepoError> {
    let mut stmt = conn.prepare("SELECT id, project_id, config_json FROM jobs")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut due = Vec::new();
    for row in rows {
        let (id, project_id, json) = row?;
        let job = parse_job_row(&id, &project_id, &json)?;
        if job.next_run_at.map(|next| next <= now).unwrap_or(false) {
            due.push(job);
        }
    }
    Ok(due)
}

pub(super) fn set_job_next_run_q(
    conn: &Connection,
    job_id: Uuid,
    next_run_at: Option<chrono::DateTime<Utc>>,
    last_enqueued_at: Option<chrono::DateTime<Utc>>,
) -> Result<(), RepoError> {
    let mut job = get_job_q(conn, job_id)?.ok_or(RepoError::InvalidCredentials)?;
    job.next_run_at = next_run_at;
    job.last_enqueued_at = last_enqueued_at;
    conn.execute(
        "UPDATE jobs SET config_json = ?1 WHERE id = ?2",
        params![serde_json::to_string(&job)?, job_id.to_string()],
    )?;
    Ok(())
}

fn parse_job_row(id: &str, project_id: &str, json: &str) -> Result<Job, RepoError> {
    let row_id = Uuid::parse_str(id)?;
    let row_project_id = Uuid::parse_str(project_id)?;
    let job: Job = serde_json::from_str(json)?;
    if job.id != row_id {
        return Err(RepoError::InvalidColumn(
            "jobs.config_json.id does not match row id".to_string(),
        ));
    }
    if job.project_id != row_project_id {
        return Err(RepoError::InvalidColumn(
            "jobs.config_json.project_id does not match row project_id".to_string(),
        ));
    }
    Ok(job)
}

impl SqliteStore {
    pub fn put_job(&self, job: Job) -> Result<Job, StateError> {
        let connection = self.connection()?;
        put_job_q(&connection, &job).map_err(StateError::from)?;
        Ok(job)
    }

    pub fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, StateError> {
        let connection = self.connection()?;
        get_job_q(&connection, job_id).map_err(StateError::from)
    }

    pub fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, StateError> {
        let connection = self.connection()?;
        list_jobs_q(&connection, project_id).map_err(StateError::from)
    }

    pub fn delete_job(&self, job_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_job_q(&connection, job_id).map_err(StateError::from)
    }

    pub fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, StateError> {
        let connection = self.connection()?;
        create_job_run_q(&connection, job_id).map_err(StateError::from)
    }

    pub fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, StateError> {
        let connection = self.connection()?;
        list_job_runs_q(&connection, job_id).map_err(StateError::from)
    }

    pub fn update_job_run(
        &self,
        run_id: Uuid,
        status: JobRunStatus,
        exit_code: Option<i32>,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        update_job_run_q(&connection, run_id, status, exit_code).map_err(StateError::from)
    }

    pub fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, StateError> {
        let connection = self.connection()?;
        active_run_q(&connection, job_id).map_err(StateError::from)
    }

    pub fn fail_orphan_runs(&self) -> Result<usize, StateError> {
        let connection = self.connection()?;
        fail_orphan_runs_q(&connection).map_err(StateError::from)
    }

    pub fn claim_due_jobs(&self, now: chrono::DateTime<Utc>) -> Result<Vec<Job>, StateError> {
        let connection = self.connection()?;
        claim_due_jobs_q(&connection, now).map_err(StateError::from)
    }

    pub fn set_job_next_run(
        &self,
        job_id: Uuid,
        next_run_at: Option<chrono::DateTime<Utc>>,
        last_enqueued_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        set_job_next_run_q(&connection, job_id, next_run_at, last_enqueued_at)
            .map_err(StateError::from)
    }
}

#[derive(Clone)]
pub struct SqliteJobRepo {
    pool: SqlitePool,
}

impl SqliteJobRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteJobRepo {
    pub fn put_job(&self, job: Job) -> Result<Job, RepoError> {
        let conn = self.pool.connection()?;
        put_job_q(&conn, &job)?;
        Ok(job)
    }

    pub fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, RepoError> {
        let conn = self.pool.connection()?;
        get_job_q(&conn, job_id)
    }

    pub fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, RepoError> {
        let conn = self.pool.connection()?;
        list_jobs_q(&conn, project_id)
    }

    pub fn delete_job(&self, job_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_job_q(&conn, job_id)
    }

    pub fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, RepoError> {
        let conn = self.pool.connection()?;
        create_job_run_q(&conn, job_id)
    }

    pub fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, RepoError> {
        let conn = self.pool.connection()?;
        list_job_runs_q(&conn, job_id)
    }

    pub fn update_job_run(
        &self,
        run_id: Uuid,
        status: JobRunStatus,
        exit_code: Option<i32>,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        update_job_run_q(&conn, run_id, status, exit_code)
    }

    pub fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, RepoError> {
        let conn = self.pool.connection()?;
        active_run_q(&conn, job_id)
    }

    pub fn fail_orphan_runs(&self) -> Result<usize, RepoError> {
        let conn = self.pool.connection()?;
        fail_orphan_runs_q(&conn)
    }

    pub fn claim_due_jobs(&self, now: chrono::DateTime<Utc>) -> Result<Vec<Job>, RepoError> {
        let conn = self.pool.connection()?;
        claim_due_jobs_q(&conn, now)
    }

    pub fn set_job_next_run(
        &self,
        job_id: Uuid,
        next_run_at: Option<chrono::DateTime<Utc>>,
        last_enqueued_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        set_job_next_run_q(&conn, job_id, next_run_at, last_enqueued_at)
    }
}
