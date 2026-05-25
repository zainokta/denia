# Jobs UI (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a jobs UI: list/create/delete jobs, show schedules, trigger manual runs, and view run history with status + exit code, on the Effect + TanStack Query layer.

**Architecture:** New `ApiClient` jobs/runs methods (typed Effects + Schema) bridged into Query via `runQuery`. File routes `/jobs` and `/jobs/$jobId`. The list is scoped by active `?project=` and uses backend summaries (`latest_run`, `next_run_at`) instead of N+1 run-history calls. Run status renders as DESIGN.md signal colors; run-now is role-gated via the RBAC companion's `useAuth`. Run-history polls while a run is in progress.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-jobs-frontend.md`. Depends on the jobs backend + projects (B) + operator-console + RBAC (C) companions.

---

## File Structure

- `web/src/effect/schema.ts` — `Job`, `JobRun`, `JobRunStatus`.
- `web/src/effect/api-client.ts` — jobs/runs methods.
- `web/src/components/RunStatusSignal.tsx` — `JobRunStatus` -> `.signal-*`.
- `web/src/routes/jobs/index.tsx` — list + create.
- `web/src/routes/jobs/$jobId.tsx` — detail + run-now + history.
- `web/src/components/Header.tsx` — `/jobs` nav link.
- Tests colocated.

Commit after each task.

---

## Task 1: Schemas

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test** — decode a `Job` (with `schedule: null` and with a cron string), a `JobSummary` (`latest_run`, nullable `next_run_at`), and a `JobRun` (status union, nullable `exit_code`).
- [ ] **Step 2: Run** `pnpm test` → FAIL.
- [ ] **Step 3: Implement** — `JobRunStatus = Schema.Literal('Pending','Running','Succeeded','Failed','Skipped')`; `Job` (`Schema.Class`) with `project_id`, `schedule: Schema.NullOr(Schema.String)`, `next_run_at: Schema.NullOr(Schema.String)`, `max_retries: Schema.Number`, env tuples; `JobRun` with `attempt`, `exit_code: Schema.NullOr(Schema.Number)`, nullable timestamps; `JobSummary` with `job`, `latest_run`, `next_run_at`. Arrays `JobSummaries`, `JobRuns`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): job + run schemas"`

---

## Task 2: ApiClient jobs methods

**Files:**
- Modify: `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `listJobs(projectId)` calls `/v1/jobs?project_id=<id>` and decodes summaries from a stub `HttpClient`; `runJob` maps a 409 to a distinguishable `ApiError`; create with bad cron maps a 400.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — add `listJobs(projectId)`, `getJob(id)`, `createJob(input)`, `deleteJob(id)`, `runJob(id)`, `listJobRuns(id)` to the `ApiClient` shape + `ApiClientLive`. Bearer from `AppConfig` current-token accessor; Schema decode; carry status on `ApiError` (400/409).
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ApiClient jobs methods"`

---

## Task 3: RunStatusSignal

**Files:**
- Create: `web/src/components/RunStatusSignal.tsx`
- Test: `web/src/components/RunStatusSignal.test.tsx`

- [ ] **Step 1: Write failing test** — `Succeeded`->`signal-ok`, `Failed`->`signal-fault`, `Running`/`Pending`->`signal-warn`, `Skipped`->muted.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — pure mapping component rendering the DESIGN.md `.signal-*` class + label. No color literals.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): RunStatusSignal"`

---

## Task 4: Jobs list + create route

**Files:**
- Create: `web/src/routes/jobs/index.tsx`
- Test: `web/src/routes/jobs/index.test.tsx`

- [ ] **Step 1: Write failing test** — with active `?project=p1`, renders jobs from a mocked Query (name, UTC schedule, `latest_run` signal, next fire); create form sends `project_id=p1`; invalid env/secret-looking values surface inline; create is hidden for a viewer role. With no active project, it shows a select-project empty state and does not call `listJobs`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `createFileRoute('/jobs/')`; read active `?project=`; Query key `['jobs', projectId]` calling `listJobs(projectId)` only when set; `.panel` rows with `RunStatusSignal` for `latest_run` + UTC cron string + backend `next_run_at` when present; create form (name, source, command, non-secret env, schedule, max_retries) -> mutation including active `project_id`, invalidate `['jobs', projectId]`; gate create behind `can('operator', roleForActiveProject(projectId))`. Empty/select-project states. Form copy must point secrets to credential/secret references, not raw env values.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): jobs list + create"`

---

## Task 5: Job detail — run-now + history

**Files:**
- Create: `web/src/routes/jobs/$jobId.tsx`
- Test: `web/src/routes/jobs/$jobId.test.tsx`

- [ ] **Step 1: Write failing test** — renders schedule + run history (status signal, attempt, exit code, tabular times); run-now calls the mutation; a 409 shows the inline "run in progress" message; delete while active 409 shows inline; run-now hidden for a viewer.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `createFileRoute('/jobs/$jobId')`; `['jobs', id]` + `['jobs', id, 'runs']` Queries; schedule `.panel` (UTC cron + backend `next_run_at` when present + optional human hint); run-now `.btn-primary` -> `runJob` mutation (invalidate runs and `['jobs', projectId]`), 409 inline; delete mutation 409 inline; run-history table with `RunStatusSignal`. Poll the runs Query (`refetchInterval` ~2s) while the newest run is `Pending|Running`, stop otherwise; gate run-now/delete behind `can('operator', roleForActiveProject(job.project_id))`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): job detail, run-now, history"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — schemas, ApiClient, RunStatusSignal, routes green.
- [ ] `cd web && pnpm build`.
- [ ] Manual (backend running): create a one-off job and run it -> Running ->
  Succeeded with exit 0; create a `*/1 * * * *` job and watch runs appear;
  trigger run-now while active -> 409 inline; confirm a viewer cannot run/create.

## Notes

- Never invent a next-fire time the backend didn't provide; show the cron string.
- Run-status colors: ok/warn/violet/muted per DESIGN.md; run-now = pink primary.
- Builds on operator-console (signal), projects (active project), RBAC (gating);
  sequence after them.
