# ADR-010: Jobs and Scheduler

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

Operators want to run cron-driven or manually-triggered batch jobs against
the same Linux runtime isolation used for long-running services, with run
history (status + exit code) preserved across restarts.

## Decision

- Domain: `Job` (cron schedule + source + command + env), `JobRun` (status,
  attempt, exit_code, timestamps), `JobRunStatus`,
  `JobRunRequest`/`JobOutcome` for the runtime API surface.
- Persistence: `jobs` + `job_runs` tables (schema_version 4). `active_run`
  finds any Pending/Running run; `claim_due_jobs(now)` returns jobs whose
  `next_run_at <= now`; `fail_orphan_runs` reconciles Pending/Running rows on
  process restart; `set_job_next_run` advances the cron cursor.
- Runtime: `Runtime::run_to_completion(JobRunRequest) -> JobOutcome` is added
  to the trait. `FakeRuntime` returns a successful outcome; `LinuxRuntime`
  inherits the default `Err` placeholder until the privileged namespace
  pipeline lands a one-shot variant.
- Scheduler: `src/scheduler.rs` owns a `Scheduler` with `tick(now)` that
  drains `claim_due_jobs`, creates Pending `JobRun` rows (skipping jobs that
  already have an active run — Forbid concurrency policy), and advances each
  job's cursor via `cron::Schedule`. A 1-second `tokio::time::interval`
  drives `tick` from `run_until_shutdown`.
- Boot recovery: `main.rs` calls `fail_orphan_runs` on startup to reconcile
  Pending/Running rows left by a crash.
- API: `POST /v1/jobs/{id}/run` returns `409 Conflict` if `active_run` finds
  an in-progress run, else creates a Pending row and returns `202 Accepted`.
- Graceful shutdown: `axum::serve(...).with_graceful_shutdown(ctrl_c)`
  triggers the scheduler's oneshot shutdown channel before the binary
  exits.

## Consequences

- The scheduler is in-process. A single Denia binary owns all cron evaluation
  for the node; this matches the single-node design.
- `run_to_completion` is wired into the trait so the call sites compile, but
  the privileged Linux execution path (mounts + uid map + cgroup + wait) is
  intentionally deferred. The error variant returned by the default is
  `RuntimeError::InvalidServiceName` (re-used to keep the trait surface
  small); a dedicated `RuntimeError::NotImplemented` variant can be added
  when the privileged path lands.
- Run reconciliation on boot is destructive: a Pending row from a clean
  shutdown that hadn't yet been executed will be marked Failed. Acceptable
  because the scheduler will re-evaluate the cron cursor at the next tick.

## Alternatives Considered

- **Reusing `start()` with a wait flag**: rejected; `RuntimeStartRequest`
  carries socket/internal-port semantics that don't apply to one-shot runs.
- **OS cron in a sidecar**: rejected; one persistent control plane should
  own all scheduling so run history stays in SQLite.

## References

- `docs/superpowers/plans/2026-05-25-jobs.md`
- `docs/superpowers/specs/2026-05-25-jobs.md`
