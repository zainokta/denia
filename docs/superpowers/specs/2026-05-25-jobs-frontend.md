# Spec: Jobs UI (Frontend) — companion to jobs

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-jobs.md`](2026-05-25-jobs.md)

## Problem

The backend adds run-to-completion + scheduled jobs (`/v1/jobs`, JobRuns). The
console has no UI to create jobs, trigger a run, or inspect run history and exit
codes.

## Goal

A jobs surface in the console: list/create/delete jobs, show the schedule,
trigger a manual run, and view per-job run history with status + exit code.
Effect + Query layer, same-origin `/v1`, DESIGN.md system, role-gated via RBAC
when present.

## Backend surface consumed

- `GET /v1/jobs?project_id=...` returns summaries with `latest_run` and
  `next_run_at` so the list view does not issue one run-history request per job.
- `POST /v1/jobs`, `GET/DELETE /v1/jobs/{id}`
- `POST /v1/jobs/{id}/run` -> 202 + run id (409 if a run is active, Forbid policy)
- `GET /v1/jobs/{id}/runs` -> run history

## Decisions

- **Effect first:** jobs/runs are `ApiClient` methods (typed Effects + Schema);
  React via `runQuery`/Query. Schema `Job`, `JobRun`, `JobRunStatus`.
- **Routes** (`web/src/routes/`): `/jobs` (list + create), `/jobs/$jobId`
  (detail: schedule, run-now, run history).
- **Run status as signal (DESIGN.md):** `Succeeded`->ok, `Running`/`Pending`->
  warn, `Failed`->Breakdown violet, `Skipped`->muted. Run-now is the pink primary
  action.
- **Schedule display:** show the raw cron string + a human hint
  (client-side parse for "every minute" etc is optional; schedules are UTC; never
  invent a next-fire time the backend didn't provide).
- **Run-now while active:** 409 surfaces inline ("a run is already in progress"),
  not a crash.
- **Role gating:** run-now / create / delete require `operator` on the job's
  project (reuse `useAuth().roleForActiveProject` + the rank map from the RBAC
  companion); viewers see read-only.
- **Project scoping:** jobs list reads the active `?project=`. If no active
  project is selected, show an empty/select-project state and hide create/run
  actions instead of querying all jobs.
- **Polling:** the run-history Query polls faster while the newest run is
  non-terminal (Pending/Running), stops otherwise; paused when the tab is hidden.

## Components / data flow

- `ApiClient`: `listJobs(projectId)`, `getJob`, `createJob`, `deleteJob`,
  `runJob(id)`, `listJobRuns(id)`.
- `web/src/routes/jobs/index.tsx` — jobs `.panel` list (name, schedule, last-run
  status signal from `latest_run`, next fire when `next_run_at` is present) +
  create form (name, active `project_id`, source, command, env, schedule,
  max_retries).
- `web/src/routes/jobs/$jobId.tsx` — schedule block, run-now button, run-history
  table (status signal, attempt, exit code, started/finished, tabular times).
- Reuse a run-status -> signal mapping (a small `RunStatusSignal`, or the
  console's `StatusSignal` extended).

## Errors / edge cases

- Invalid cron on create -> 400 surfaced inline on the form.
- Invalid env key / secret-looking env value -> 400 surfaced inline; copy should
  direct users to credential/secret references rather than raw secrets.
- Run-now when active -> 409 inline.
- Delete while active -> 409 inline.
- Empty states: no jobs; a job with no runs yet.
- 401 -> auth-needed (RBAC companion handles the redirect).

## Success criteria

- Operator creates a job (one-off or scheduled), triggers a manual run, and sees
  it move Running -> Succeeded/Failed with the exit code.
- Run history is visible per job; scheduled jobs show their cron.
- Viewers can read but not run/create/delete.

## Testing

- `@effect/vitest`: jobs/runs `ApiClient` methods + Schema; 400 (bad cron) and
  409 (active) error mapping.
- `@testing-library/react`: list + create; run-now calls the mutation and shows
  409 inline; run-history renders status signals + exit codes; run-now hidden for
  a viewer; no active project hides create/run and does not fetch all jobs.

## Out of scope

Job DAGs/dependencies, array/parallel jobs, live run-log streaming, timeouts UI.
Backend behaviour (its own spec). Builds on the operator-console (status signal),
projects (active project), and RBAC (gating) companions.
