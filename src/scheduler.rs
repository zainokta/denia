use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use cron::Schedule;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::domain::{Job, JobRun, JobRunStatus};
use crate::state::{SqliteStore, StateError};

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("state error: {0}")]
    State(#[from] StateError),
}

pub struct Scheduler {
    store: SqliteStore,
    manual_tx: mpsc::UnboundedSender<JobRun>,
}

impl Scheduler {
    pub fn new(store: SqliteStore) -> (Self, mpsc::UnboundedReceiver<JobRun>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                store,
                manual_tx: tx,
            },
            rx,
        )
    }

    pub fn enqueue_manual(&self, run: JobRun) {
        let _ = self.manual_tx.send(run);
    }

    pub fn tick(&self, now: DateTime<Utc>) -> Result<Vec<JobRun>, SchedulerError> {
        let due = self.store.claim_due_jobs(now)?;
        let mut enqueued = Vec::new();
        for job in due {
            if self.store.active_run(job.id)?.is_some() {
                continue;
            }
            let run = self.store.create_job_run(job.id)?;
            let next = compute_next_run(&job, now);
            self.store.set_job_next_run(job.id, next, Some(now))?;
            enqueued.push(run);
        }
        Ok(enqueued)
    }

    pub fn store(&self) -> &SqliteStore {
        &self.store
    }
}

pub fn compute_next_run(job: &Job, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let schedule = job.schedule.as_ref()?;
    let parsed: Schedule = schedule.parse().ok()?;
    parsed.after(&after).next()
}

pub async fn run_until_shutdown(
    scheduler: Arc<Scheduler>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let _ = scheduler.tick(Utc::now());
            }
        }
    }
}

pub fn mark_run(
    store: &SqliteStore,
    run_id: uuid::Uuid,
    status: JobRunStatus,
    exit_code: Option<i32>,
) -> Result<(), StateError> {
    store.update_job_run(run_id, status, exit_code)
}
