use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cron::Schedule;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::domain::{Job, JobRun, JobRunRequest, JobRunStatus};
use crate::runtime::Runtime;
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

    /// Hand a run to the executor channel. A dropped receiver (executor not
    /// wired) is logged rather than silently swallowed so a misconfiguration is
    /// visible instead of jobs sitting Pending forever.
    pub fn enqueue_manual(&self, run: JobRun) {
        if self.manual_tx.send(run).is_err() {
            tracing::error!("job executor channel closed; manual run will not execute");
        }
    }

    /// A sender handle the API uses to enqueue manually-triggered runs onto the
    /// same channel the executor drains.
    pub fn manual_sender(&self) -> mpsc::UnboundedSender<JobRun> {
        self.manual_tx.clone()
    }

    pub fn tick(&self, now: DateTime<Utc>) -> Result<Vec<JobRun>, SchedulerError> {
        let due = self.store.claim_due_jobs(now)?;
        let mut enqueued = Vec::new();
        for job in due {
            // Forbid concurrency (ADR-010): if a run is already active, record a
            // Skipped run for history and advance the cursor so the same fire
            // time is not re-claimed every tick.
            if self.store.active_run(job.id)?.is_some() {
                self.store.create_skipped_run(job.id)?;
                let next = compute_next_run(&job, now);
                self.store.set_job_next_run(job.id, next, Some(now))?;
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

/// Drives cron evaluation: each second `tick` claims due jobs, persists Pending
/// runs, and forwards them to the executor channel. The loop catches a panic
/// from `tick` so a single bad evaluation can never permanently kill scheduling
/// for the rest of the process lifetime (a panicking tokio task dies silently).
pub async fn run_until_shutdown(
    scheduler: Arc<Scheduler>,
    enqueue_tx: mpsc::UnboundedSender<JobRun>,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                let now = Utc::now();
                let scheduler = scheduler.clone();
                let result = tokio::task::spawn_blocking(move || scheduler.tick(now)).await;
                match result {
                    Ok(Ok(runs)) => {
                        for run in runs {
                            if enqueue_tx.send(run).is_err() {
                                tracing::error!("job executor channel closed; scheduled run dropped");
                            }
                        }
                    }
                    Ok(Err(error)) => tracing::warn!(?error, "scheduler tick failed"),
                    Err(error) => tracing::error!(?error, "scheduler tick panicked; loop continues"),
                }
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

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("job not found: {0}")]
    JobNotFound(uuid::Uuid),
    #[error("{0}")]
    Other(String),
}

/// Resolves a [`Job`] into a runnable [`JobRunRequest`]: acquires the rootfs
/// bundle for the job's source and computes its resource budget. Abstracted so
/// the executor is unit-testable without the privileged runtime or a real
/// registry pull.
#[async_trait]
pub trait JobRunRequestResolver: Send + Sync {
    async fn resolve(&self, job: &Job, run_id: uuid::Uuid) -> Result<JobRunRequest, ResolveError>;
}

/// Outcome of attempting to run a single job run, before retry policy is
/// applied. `exit_code == 0` is success; any other code or a runtime/resolve
/// error is a failed attempt.
enum AttemptOutcome {
    Succeeded { exit_code: i32 },
    Failed { exit_code: Option<i32>, reason: String },
}

/// Consumes the scheduler's run channel and executes each [`JobRun`] against
/// the runtime via [`Runtime::run_to_completion`], applying ADR-010 retry
/// semantics: a run is retried up to `job.max_retries` times (reusing the run
/// row, incrementing its `attempt`), succeeding on exit 0 and failing
/// otherwise.
pub struct JobExecutor {
    store: SqliteStore,
    runtime: Arc<dyn Runtime>,
    resolver: Arc<dyn JobRunRequestResolver>,
}

impl JobExecutor {
    pub fn new(
        store: SqliteStore,
        runtime: Arc<dyn Runtime>,
        resolver: Arc<dyn JobRunRequestResolver>,
    ) -> Self {
        Self {
            store,
            runtime,
            resolver,
        }
    }

    /// Execute one run to a terminal state (Succeeded or Failed), honoring the
    /// job's retry budget. Best-effort: any persistence/runtime error is mapped
    /// to a failed attempt and never propagated, so one bad run cannot crash the
    /// executor loop.
    pub async fn execute(&self, run: JobRun) {
        let run_id = run.id;
        let job = match self.store.get_job(run.job_id) {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::warn!(job_id = %run.job_id, %run_id, "job run for unknown job; failing");
                let _ = self
                    .store
                    .update_job_run(run_id, JobRunStatus::Failed, None);
                return;
            }
            Err(error) => {
                tracing::warn!(?error, %run_id, "failed to load job for run; failing");
                let _ = self
                    .store
                    .update_job_run(run_id, JobRunStatus::Failed, None);
                return;
            }
        };

        // attempt counts from 1; max_retries additional attempts are allowed.
        let max_attempts = job.max_retries.saturating_add(1);
        let mut last_exit: Option<i32> = None;
        for attempt in 1..=max_attempts {
            if let Err(error) = self.store.start_job_run(run_id, attempt) {
                tracing::warn!(?error, %run_id, "failed to mark job run Running");
            }
            match self.run_attempt(&job, run_id).await {
                AttemptOutcome::Succeeded { exit_code } => {
                    let _ = self.store.update_job_run(
                        run_id,
                        JobRunStatus::Succeeded,
                        Some(exit_code),
                    );
                    tracing::info!(job_id = %job.id, %run_id, attempt, "job run succeeded");
                    return;
                }
                AttemptOutcome::Failed { exit_code, reason } => {
                    last_exit = exit_code;
                    tracing::warn!(
                        job_id = %job.id,
                        %run_id,
                        attempt,
                        max_attempts,
                        exit_code = ?exit_code,
                        %reason,
                        "job run attempt failed"
                    );
                }
            }
        }

        let _ = self
            .store
            .update_job_run(run_id, JobRunStatus::Failed, last_exit);
        tracing::warn!(job_id = %job.id, %run_id, "job run exhausted retries; marked Failed");
    }

    async fn run_attempt(&self, job: &Job, run_id: uuid::Uuid) -> AttemptOutcome {
        let request = match self.resolver.resolve(job, run_id).await {
            Ok(request) => request,
            Err(error) => {
                return AttemptOutcome::Failed {
                    exit_code: None,
                    reason: format!("resolve failed: {error}"),
                };
            }
        };
        match self.runtime.run_to_completion(request).await {
            Ok(outcome) if outcome.exit_code == 0 => AttemptOutcome::Succeeded {
                exit_code: outcome.exit_code,
            },
            Ok(outcome) => AttemptOutcome::Failed {
                exit_code: Some(outcome.exit_code),
                reason: format!("nonzero exit {}", outcome.exit_code),
            },
            Err(error) => AttemptOutcome::Failed {
                exit_code: None,
                reason: format!("runtime error: {error}"),
            },
        }
    }
}

/// Drain the run channel and execute each run. Runs are executed serially
/// (single-node, in-process): the channel is the work queue and Forbid
/// concurrency is enforced upstream by `active_run`. Returns when the channel
/// closes (all senders dropped at shutdown).
pub async fn run_executor(executor: Arc<JobExecutor>, mut rx: mpsc::UnboundedReceiver<JobRun>) {
    while let Some(run) = rx.recv().await {
        executor.execute(run).await;
    }
}

/// Production [`JobRunRequestResolver`]: acquires a rootfs bundle for the job's
/// source (reusing the deploy artifact acquirer and the same registry-auth
/// resolution) and derives the job's resource budget from its project's
/// defaults. The bundle is materialized under `artifact_dir`, exactly where
/// `LinuxRuntime::run_to_completion` reads it.
pub struct RuntimeJobRunRequestResolver {
    config: crate::config::AppConfig,
    repos: crate::deploy::DeploymentRepos,
    projects: crate::repo::sqlite::SqliteProjectRepo,
    runner: Arc<dyn crate::command::CommandRunner>,
    oci_cache: Option<crate::oci::cache::LayerCache>,
}

impl RuntimeJobRunRequestResolver {
    pub fn new(
        config: crate::config::AppConfig,
        repos: crate::deploy::DeploymentRepos,
        projects: crate::repo::sqlite::SqliteProjectRepo,
        runner: Arc<dyn crate::command::CommandRunner>,
        oci_cache: Option<crate::oci::cache::LayerCache>,
    ) -> Self {
        Self {
            config,
            repos,
            projects,
            runner,
            oci_cache,
        }
    }
}

#[async_trait]
impl JobRunRequestResolver for RuntimeJobRunRequestResolver {
    async fn resolve(&self, job: &Job, run_id: uuid::Uuid) -> Result<JobRunRequest, ResolveError> {
        use crate::artifacts::acquirer::{ArtifactAcquireRequest, ArtifactAcquirer};
        use crate::domain::ServiceSource;
        use crate::oci::RegistryAuth;

        let acquirer = match self.oci_cache.clone() {
            Some(cache) => ArtifactAcquirer::new_with_cache(self.config.clone(), cache),
            None => ArtifactAcquirer::new(self.config.clone()),
        };

        // Resolve the acquire request + auth from the job's source, reusing the
        // deploy path's external-image auth resolution (registry rows + SOPS).
        let (request, auth) = match &job.source {
            ServiceSource::ExternalImage(source) => {
                let secret_store =
                    crate::secrets::SopsSecretStore::new(self.config.data_dir.clone());
                let (full_ref, auth) = crate::deploy::coordinator::resolve_external_auth(
                    &self.repos,
                    source,
                    job.project_id,
                    &secret_store,
                    self.runner.as_ref(),
                    &self.config.sops_binary,
                    &self.config.age_key_file,
                )
                .await
                .map_err(|e| ResolveError::Other(format!("auth resolution failed: {e}")))?;
                (
                    ArtifactAcquireRequest::ExternalImage { image: full_ref },
                    auth,
                )
            }
            ServiceSource::Git(source) => (
                ArtifactAcquireRequest::Git {
                    repo_url: source.repo_url.clone(),
                    git_ref: source.git_ref.clone(),
                    dockerfile_path: source.dockerfile_path.clone(),
                    context_path: source.context_path.clone(),
                },
                RegistryAuth::Anonymous,
            ),
        };

        let artifact = acquirer
            .acquire_rootfs_bundle_from_image_config(self.runner.as_ref(), request, auth)
            .await
            .map_err(|e| ResolveError::Other(format!("artifact acquisition failed: {e}")))?;

        // Resource budget + env: jobs inherit the project's shared env and its
        // default resource limits (or the global default), mirroring how plain
        // services derive `effective_limits`/`effective_env`. The job's own env
        // overrides shared keys.
        let (limits, env) = match self.projects.get_project(job.project_id) {
            Ok(Some(project)) => {
                let limits = project
                    .default_resource_limits
                    .clone()
                    .unwrap_or_default();
                let mut env: std::collections::BTreeMap<String, String> =
                    project.shared_env.iter().cloned().collect();
                env.extend(job.env.iter().cloned());
                (limits, env.into_iter().collect::<Vec<_>>())
            }
            _ => (crate::domain::ResourceLimits::default(), job.env.clone()),
        };

        Ok(JobRunRequest {
            job_id: job.id,
            run_id,
            artifact,
            command: job.command.clone(),
            env,
            cpu_millis: limits.cpu_millis,
            memory_bytes: limits.memory_bytes,
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::domain::{ExternalImageSource, JobOutcome, ServiceSource};
    use crate::runtime::RuntimeError;
    use std::sync::Mutex;
    use uuid::Uuid;

    fn external_image_source() -> ServiceSource {
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "busybox".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        })
    }

    fn rootfs_artifact() -> ArtifactRecord {
        ArtifactRecord::new(
            "sha256:deadbeef",
            ArtifactKind::RootfsBundle,
            ArtifactSource::ExternalRegistry {
                image: "busybox".into(),
            },
        )
        .unwrap()
    }

    /// Resolver returning a canned request, so the executor can be driven
    /// without acquiring a real rootfs bundle.
    struct CannedResolver;

    #[async_trait]
    impl JobRunRequestResolver for CannedResolver {
        async fn resolve(
            &self,
            job: &Job,
            run_id: Uuid,
        ) -> Result<JobRunRequest, ResolveError> {
            Ok(JobRunRequest {
                job_id: job.id,
                run_id,
                artifact: rootfs_artifact(),
                command: job.command.clone(),
                env: job.env.clone(),
                cpu_millis: 500,
                memory_bytes: 512 * 1024 * 1024,
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
            })
        }
    }

    /// Runtime whose `run_to_completion` returns a scripted sequence of exit
    /// codes, recording how many times it was invoked.
    #[derive(Default)]
    struct ScriptedRuntime {
        exit_codes: Mutex<std::collections::VecDeque<i32>>,
        calls: Mutex<u32>,
    }

    impl ScriptedRuntime {
        fn new(codes: Vec<i32>) -> Arc<Self> {
            Arc::new(Self {
                exit_codes: Mutex::new(codes.into_iter().collect()),
                calls: Mutex::new(0),
            })
        }
        fn call_count(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl Runtime for ScriptedRuntime {
        async fn start(
            &self,
            _request: crate::domain::RuntimeStartRequest,
        ) -> Result<crate::domain::RuntimeStatus, RuntimeError> {
            Err(RuntimeError::InvalidServiceName {
                name: "unused".into(),
            })
        }
        async fn stop(
            &self,
            _instance: &crate::domain::RuntimeInstanceId,
        ) -> Result<(), RuntimeError> {
            Ok(())
        }
        async fn run_to_completion(
            &self,
            _request: JobRunRequest,
        ) -> Result<JobOutcome, RuntimeError> {
            *self.calls.lock().unwrap() += 1;
            let code = self.exit_codes.lock().unwrap().pop_front().unwrap_or(0);
            let now = Utc::now();
            Ok(JobOutcome {
                exit_code: code,
                started_at: now,
                finished_at: now,
            })
        }
    }

    fn store_with_job() -> (SqliteStore, Job) {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project_id = store.default_project_id().unwrap();
        let mut job = Job::new(project_id, "nightly", external_image_source(), None).unwrap();
        job.command = Some(vec!["/bin/true".into()]);
        let job = store.put_job(job).unwrap();
        (store, job)
    }

    #[tokio::test]
    async fn executor_runs_job_to_completion_success() {
        let (store, job) = store_with_job();
        let run = store.create_job_run(job.id).unwrap();
        let runtime = ScriptedRuntime::new(vec![0]);
        let executor = JobExecutor::new(
            store.clone(),
            runtime.clone(),
            Arc::new(CannedResolver),
        );

        executor.execute(run.clone()).await;

        assert_eq!(runtime.call_count(), 1);
        let runs = store.list_job_runs(job.id).unwrap();
        let updated = runs.iter().find(|r| r.id == run.id).unwrap();
        assert_eq!(updated.status, JobRunStatus::Succeeded);
        assert_eq!(updated.exit_code, Some(0));
        assert_eq!(updated.attempt, 1);
        assert!(updated.started_at.is_some());
        assert!(updated.finished_at.is_some());
        // No active run remains: a subsequent manual trigger would be allowed.
        assert!(store.active_run(job.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn executor_retries_up_to_max_then_fails() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project_id = store.default_project_id().unwrap();
        let mut job = Job::new(project_id, "retryer", external_image_source(), None).unwrap();
        job.max_retries = 2; // 1 initial + 2 retries = 3 attempts
        let job = store.put_job(job).unwrap();
        let run = store.create_job_run(job.id).unwrap();
        // Always nonzero exit: should exhaust all attempts and fail.
        let runtime = ScriptedRuntime::new(vec![1, 1, 1, 1]);
        let executor = JobExecutor::new(store.clone(), runtime.clone(), Arc::new(CannedResolver));

        executor.execute(run.clone()).await;

        assert_eq!(runtime.call_count(), 3, "1 initial + 2 retries");
        let runs = store.list_job_runs(job.id).unwrap();
        let updated = runs.iter().find(|r| r.id == run.id).unwrap();
        assert_eq!(updated.status, JobRunStatus::Failed);
        assert_eq!(updated.exit_code, Some(1));
        assert_eq!(updated.attempt, 3);
    }

    #[tokio::test]
    async fn executor_succeeds_on_retry() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project_id = store.default_project_id().unwrap();
        let mut job = Job::new(project_id, "flaky", external_image_source(), None).unwrap();
        job.max_retries = 3;
        let job = store.put_job(job).unwrap();
        let run = store.create_job_run(job.id).unwrap();
        // Fail twice, then succeed on the third attempt.
        let runtime = ScriptedRuntime::new(vec![1, 1, 0]);
        let executor = JobExecutor::new(store.clone(), runtime.clone(), Arc::new(CannedResolver));

        executor.execute(run.clone()).await;

        assert_eq!(runtime.call_count(), 3);
        let runs = store.list_job_runs(job.id).unwrap();
        let updated = runs.iter().find(|r| r.id == run.id).unwrap();
        assert_eq!(updated.status, JobRunStatus::Succeeded);
        assert_eq!(updated.exit_code, Some(0));
        assert_eq!(updated.attempt, 3);
    }

    #[tokio::test]
    async fn tick_primes_then_skips_when_active_and_records_skipped() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let project_id = store.default_project_id().unwrap();
        // A scheduled job, primed so it is due now.
        let mut job = Job::new(
            project_id,
            "cron",
            external_image_source(),
            Some("* * * * * *".to_string()),
        )
        .unwrap();
        job.next_run_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let job = store.put_job(job).unwrap();

        let (scheduler, _rx) = Scheduler::new(store.clone());

        // First tick claims the due job and enqueues a Pending run.
        let enqueued = scheduler.tick(Utc::now()).unwrap();
        assert_eq!(enqueued.len(), 1);
        // The run is now active (Pending), so the next due tick must Skip it.
        let next = scheduler.tick(Utc::now()).unwrap();
        assert!(next.is_empty(), "active run blocks a new run");
        let runs = store.list_job_runs(job.id).unwrap();
        assert!(
            runs.iter().any(|r| r.status == JobRunStatus::Skipped),
            "a Skipped run was recorded for the suppressed fire: {runs:?}"
        );
    }

    #[tokio::test]
    async fn unknown_job_run_is_failed() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        let orphan = JobRun {
            id: Uuid::now_v7(),
            job_id: Uuid::now_v7(),
            status: JobRunStatus::Pending,
            attempt: 1,
            exit_code: None,
            started_at: None,
            finished_at: None,
            created_at: Utc::now(),
        };
        let runtime = ScriptedRuntime::new(vec![0]);
        let executor = JobExecutor::new(store.clone(), runtime.clone(), Arc::new(CannedResolver));
        executor.execute(orphan).await;
        // No runtime invocation for a job that does not exist.
        assert_eq!(runtime.call_count(), 0);
    }
}
