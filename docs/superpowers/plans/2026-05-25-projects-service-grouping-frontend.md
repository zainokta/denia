# Projects UI (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a project management UI to the web console: list/create/view/delete projects, edit shared config, and a project switcher, built on the Effect + TanStack Query layer.

**Architecture:** New `ApiClient` project methods (typed Effects + Schema) bridged into TanStack Query via `runQuery`. File-based routes `/projects` and `/projects/$projectId`. A `ProjectSwitcher` in the header stores the active project in the `?project=` search param. All UI uses the DESIGN.md primitives.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`, Tailwind v4. Spec: `docs/superpowers/specs/2026-05-25-projects-service-grouping-frontend.md`. Depends on backend `/v1/projects`.

---

## File Structure

- `web/src/effect/schema.ts` — add `Project`, `ProjectInput` schemas.
- `web/src/effect/api-client.ts` — add `listProjects/getProject/createProject/deleteProject`.
- `web/src/routes/projects/index.tsx` — list + create.
- `web/src/routes/projects/$projectId.tsx` — detail + shared-config editor + members.
- `web/src/components/ProjectSwitcher.tsx` — header switcher.
- `web/src/components/Header.tsx` — mount the switcher + nav link.
- Tests colocated: `web/src/effect/api-client.test.ts` (extend), `*.test.tsx` per route/component.

Commit after each task.

---

## Task 1: Project schema

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test**

```ts
it.effect('decodes a project', () =>
  Schema.decodeUnknownEffect(Project)({
    id: '018f-...', name: 'web', description: null,
    shared_env: [['A', '1']], default_resource_limits: null,
    created_at: '2026-05-25T00:00:00Z',
  }).pipe(Effect.map((p) => { expect(p.name).toBe('web') })))
```

- [ ] **Step 2: Run, verify fail** — `pnpm test` → FAIL (no `Project`).

- [ ] **Step 3: Implement** in `schema.ts`

```ts
export class Project extends Schema.Class<Project>('Project')({
  id: Schema.String,
  name: Schema.String,
  description: Schema.NullOr(Schema.String),
  shared_env: Schema.Array(Schema.Tuple(Schema.String, Schema.String)),
  default_resource_limits: Schema.NullOr(
    Schema.Struct({ cpu_millis: Schema.Number, memory_bytes: Schema.Number }),
  ),
  created_at: Schema.String,
}) {}
export const Projects = Schema.Array(Project)
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): project schema"`

---

## Task 2: ApiClient project methods

**Files:**
- Modify: `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `listProjects` decodes an array from a stub `HttpClient`; a 409 delete maps to `ApiError`. Provide a stub fetch via `FetchHttpClient.Fetch` or a stub `HttpClient` layer (mirror existing test setup).
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — add to the `ApiClient` service shape and `ApiClientLive`:
  - `listProjects: Effect<ReadonlyArray<Project>, ApiError | DecodeError>` → `GET {base}/v1/projects`.
  - `getProject(id)`, `createProject(input)` (`POST`), `deleteProject(id)` (`DELETE`).
  - Reuse the bearer header from `AppConfig.token`; decode with `Schema`; map HTTP/status errors to `ApiError`, decode failures to `DecodeError`. Surface 409 as a distinguishable `ApiError` (carry status).
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ApiClient project methods"`

---

## Task 3: Projects list route

**Files:**
- Create: `web/src/routes/projects/index.tsx`
- Test: `web/src/routes/projects/index.test.tsx`

- [ ] **Step 1: Write failing test** — renders project names from a mocked Query result; clicking create calls the mutation.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `createFileRoute('/projects/')`; `useQuery({ queryKey: ['projects'], queryFn: () => runQuery(listProjectsProgram) })`; flat `.panel` list with a `.signal-steady` dot per project; create form (name + description) → `useMutation` calling `runQuery(createProjectProgram(input))`, invalidate `['projects']`. Empty-state copy when only `default`.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): projects list + create route"`

---

## Task 4: Project detail route + shared-config editor

**Files:**
- Create: `web/src/routes/projects/$projectId.tsx`
- Test: `web/src/routes/projects/$projectId.test.tsx`

- [ ] **Step 1: Write failing test** — renders shared-env rows + member services; delete on a non-empty project shows the 409 message.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `createFileRoute('/projects/$projectId')`; queries `['projects', id]` and `['projects', id, 'services']`; key/value editor for `shared_env`; default-limits fields; member-services `.panel` list; delete button (`.btn`, violet on hover) → mutation; on `ApiError` 409 render an inline fault `.signal-fault` message linking to members.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): project detail + shared-config editor"`

---

## Task 5: Project switcher in header

**Files:**
- Create: `web/src/components/ProjectSwitcher.tsx`
- Modify: `web/src/components/Header.tsx`
- Test: `web/src/components/ProjectSwitcher.test.tsx`

- [ ] **Step 1: Write failing test** — switcher lists projects and writes `?project=` on select.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `ProjectSwitcher` uses `['projects']` Query + `useNavigate`/`useSearch` to read/write `?project=`; styled as a `.btn`-like select. Mount in `Header` with a `/projects` nav link.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): project switcher"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — schema, ApiClient, routes, switcher green.
- [ ] `pnpm build` (client + SSR/SPA shell).
- [ ] Manual (`pnpm dev`, backend running): create a project, see it listed, open
  it, edit shared env, attempt delete while it has a service (409 message), then
  delete an empty one. Switcher updates `?project=`.

## Notes

- Mirror the existing Effect test setup in `web/src/effect/api-client.test.ts`.
- Honor DESIGN.md: no glass/gradient; color only as signal; pink = primary,
  violet = destructive/fault.
- Requires the backend `/v1/projects` endpoints (companion backend plan).
