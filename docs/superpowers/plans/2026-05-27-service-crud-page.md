# Service CRUD Page (Dokploy-style) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give operators a full create/read/update/delete experience for services in the web console: a list with create, a tabbed per-service detail page, and edit/delete, backed by real `/v1/services` endpoints.

**Architecture:** Three layers. (1) Backend gains server-generated-ID create, `GET /v1/services/{id}`, and `DELETE /v1/services/{id}`, documented in ADR-016. (2) The frontend Effect data layer is corrected to mirror the real `ServiceConfig` wire shape (UUIDv7 strings, real fields), which is a prerequisite for everything else. (3) The UI is rebuilt: list + create form, and the existing detail route is reorganised into tabs (Overview, Source, Domains, Environment, Deployments, Logs, Metrics).

**Tech Stack:** Rust 2024 + axum + SQLite (backend); TanStack Router/Query + Effect + Tailwind v4 (frontend). Design system per `DESIGN.md` ("Stagecraft and Breakdown", mono-forward, dark-primary, signal-only color).

---

## Why the data-layer fix is mandatory and first

The current `Service` schema (`web/src/effect/schema.ts:93-102`) declares `id`/`project_id` as `Schema.Number`, omits `source`/`health_check`/`resource_limits`/`env`, and invents `status`/`security` fields the backend never returns from `GET /v1/services`. Unlike `listNodes`, `listServices` (`api-client.ts:436`) has NO fixture branch: it always calls `GET /v1/services` and decodes via `Services`. So the schema already fails to decode against any real backend today (not just a "live node"). Every UI task below depends on a correct schema, so the migration is Phase 1.

**UUIDv7 invariant (CLAUDE.md):** persisted IDs must be UUIDv7. The client must NOT generate IDs (it cannot guarantee v7 and `crypto.randomUUID` is v4). Therefore **create generates the ID server-side**. We make `ServiceConfig.id` default-on-deserialize and let `put_service` resolve the id (see next section for the upsert hazard).

**Upsert hazard (must handle in `put_service`).** The persistence layer (`src/repo/sqlite/services.rs` `put_service_q`) does `INSERT ... ON CONFLICT(project_id, name) DO UPDATE SET config_json = excluded.config_json` — the row's PRIMARY KEY `id` column is never updated, only `config_json`. So if an *update* arrives with a nil id, `ensure_id` would mint a fresh v7, the INSERT would conflict on `(project_id, name)`, and the stored `config_json.id` would diverge from the row's PK id. `put_service` must therefore resolve the id by lookup, not blind assignment: **if incoming id is nil, look up the existing service by `(project_id, name)` and reuse its id; only mint `Uuid::now_v7()` when no such service exists.** The edit form (Task 8) must also always send the real id.

## Decisions baked in

- **`status` / `security` are dropped from the `Service` list shape.** They are not returned by `GET /v1/services`. Runtime status is derived from the existing `listWorkloads`: `WorkloadView.status` (`schema.ts:173`, `observability.rs:28`) is keyed by `service_id` and is `Option<DeploymentStatus>` (deployment *phase*: Pending/Building/Starting/Healthy/Failed/Stopped, or `null` when no promoted deployment). It is NOT a plain running/stopped flag. The row signal maps phase → signal class explicitly (Task 7), not by passing the raw string. Security posture has no list-level source today; the posture section renders only when a source exists (follow-up, out of scope).
- **Create = `POST /v1/services` with id omitted** (server assigns v7). **Update = `POST /v1/services` with id present** (upsert, unchanged). **Delete = new `DELETE /v1/services/{id}`.** **Read one = new `GET /v1/services/{id}`.**
- **Detail page keeps its current sections' logic**, only reorganised into tabs. No behaviour regression for deployments/logs/metrics/domains.

## File Structure

**Backend**
- Modify `src/domain/service.rs` — make `id` `#[serde(default)]` (default `Uuid::nil()`).
- Modify `src/api/services.rs` — `put_service` resolves id by lookup (reuse existing on nil, else mint v7); add `get_service` + `delete_service` handlers and routes.
- **Add `delete_service` to the services repo — it does NOT exist today.** `src/repo/sqlite/services.rs` has only `put_service_q`/`list_services_q`/`get_service_q`. Add `delete_service_q` and surface `delete_service(id)` on BOTH the `SqliteStore` facade and `SqliteServiceRepo`, mirroring `delete_project_q`/`delete_job_q`. Schema lives inline in `src/repo/sqlite/pool.rs` (services table: `id` PK, `UNIQUE(project_id, name)`); no migration file needed.
- Create `docs/adr/016-service-crud-api.md`; add row to `docs/adr/README.md`.

**Frontend (data layer)**
- Modify `web/src/effect/schema.ts` — rewrite `Service`, add `ServiceInput` (no id), reuse existing `GitSource`/`ExternalImageSource`/`ServiceSource`, `ResourceLimits`, `HealthCheck`.
- Modify `web/src/effect/api-client.ts` — retype service IDs `number → string`; add `getService`, `deleteService`; keep `putService` (now accepts create-or-update).

**Frontend (UI)**
- Modify `web/src/routes/services/index.tsx` — list with derived status, a real create-service form (replaces the deploy-only panel), delete action.
- Create `web/src/components/ServiceForm.tsx` — shared create/edit form (project select, name, domains, source git/image, port, health check, env, resource limits, TLS).
- Modify `web/src/routes/services/$serviceId.tsx` — wrap existing sections in a tab shell; add Overview + Source + edit/delete.
- Create `web/src/components/Tabs.tsx` — minimal accessible tablist (roving focus, `aria-selected`, `role="tab"/"tabpanel"`).
- Modify components consuming service IDs: `StatusSignal`, `SecurityBadge`, `TlsToggle`, `DeployPhase` only if they type IDs as number.
- Update tests: `web/src/routes/services/-index.test.tsx`, `-detail.test.tsx`, `web/src/components/*.test.tsx`.

---

## Phase 0: ADR

### Task 0: Write ADR-016

**Files:**
- Create: `docs/adr/016-service-crud-api.md`
- Modify: `docs/adr/README.md` (index table)

- [ ] **Step 1:** Write ADR-016 with sections: Status (Proposed), Date 2026-05-27, Context (no create/read-one/delete; FE schema drift; UUIDv7 invariant forces server-side id), Decision (server-generated v7 on nil id via `put_service`; add `GET`/`DELETE /v1/services/{id}`; FE schema mirrors `ServiceConfig`), Consequences, Alternatives (client-generated id — rejected for v7 invariant; separate `ServiceInput` type — rejected, default-id is lighter), References (ADR-006, ADR-008, ADR-012).
- [ ] **Step 2:** Add `| [016](016-service-crud-api.md) | Service CRUD API | Proposed | 2026-05-27 |` to the index.
- [ ] **Step 3:** Commit `docs(adr): ADR-016 service CRUD API`.

---

## Phase 1: Backend

### Task 1: Default service id for server-side resolution

**Files:**
- Modify: `src/domain/service.rs:148-163` (struct)
- Test: `src/domain/service.rs` `#[cfg(test)]`

- [ ] **Step 1: Failing test.** Assert a `ServiceConfig` deserialized from JSON without an `id` field has a nil id (proving the default applies), and one with an id keeps it.
- [ ] **Step 2:** Run `cargo test domain::service`. Expected: FAIL (id is required, deserialize errors).
- [ ] **Step 3:** Add `#[serde(default)]` to `id` (default `Uuid::nil()`). No `ensure_id` helper — id resolution is centralized in `put_service` (Task 2) because it requires a repo lookup, which the domain type cannot do.
- [ ] **Step 4:** Run test. Expected: PASS.
- [ ] **Step 5:** Commit `feat(domain): default service id to nil for server-side resolution`.

### Task 2: `put_service` id resolution; repo delete; `GET`/`DELETE` endpoints

**Files:**
- Modify: `src/api/services.rs:21-31` (router), `:60-80` (`put_service`), add `get_service`/`delete_service`
- Modify: `src/repo/sqlite/services.rs` (add `delete_service_q` + facade methods on `SqliteStore` and `SqliteServiceRepo`)
- Test: `src/api/services.rs` `#[cfg(test)]`

- [ ] **Step 1: Failing test — create without id.** POST `/v1/services` with body omitting `id`; assert 200 and the returned service `id` is a valid v7 (`Uuid::version() == Some(Version::SortRand)`); then `GET /v1/services/{id}` returns it.
- [ ] **Step 2:** Run. Expected: FAIL (no GET route; id is nil).
- [ ] **Step 3: Id resolution in `put_service`.** After source validation, resolve id without diverging from the upsert key:

```rust
if service.id.is_nil() {
    // reuse existing id for (project_id, name) so config_json.id never diverges
    // from the row PK that ON CONFLICT(project_id, name) keeps.
    service.id = match state.services.get_service_by_project_name(service.project_id, &service.name)? {
        Some(existing) => existing.id,
        None => Uuid::now_v7(),
    };
}
```

If a `get_service_by_project_name` lookup does not exist on the repo, add it (it is a thin `SELECT ... WHERE project_id=? AND name=?`), or fetch via `list_services` filtered in-handler if adding a repo method is undesirable. Then persist.

- [ ] **Step 4: Routes + handlers.** Add `.route("/services/{service_id}", get(get_service).delete(delete_service))`. `get_service`: load by id, `ensure_role(Viewer)` on its project, 404 if absent, return `Json<ServiceConfig>`. `delete_service`: load by id, `ensure_role(Operator)`, repo delete, return `{"deleted": true}`. Mirror `projects.rs` delete handler shape.
- [ ] **Step 5:** Run create+get test. Expected: PASS.
- [ ] **Step 6: Failing test — delete + update-id-stability.** (a) Create, `DELETE /v1/services/{id}` → 200, `GET` → 404, unauthenticated → 401, non-operator → 403. (b) Create a service, then POST again with same `project_id`+`name` but `id` omitted; assert the returned/stored id equals the original (no divergence).
- [ ] **Step 7: Repo delete.** Add `delete_service_q` (`DELETE FROM services WHERE id = ?`) and expose `delete_service(id)` on `SqliteStore` and `SqliteServiceRepo`. Run tests. Expected: PASS.
- [ ] **Step 8:** Run `cargo build && cargo test` and `cargo fmt --all`. Commit `feat(api): service GET/DELETE + stable server-side id on upsert`.

> Run `gitnexus_impact({target: "put_service", direction: "upstream"})` before editing; report blast radius. The deployment coordinator and registry validation already call into this path.

---

## Phase 2: Frontend data layer

### Task 3: Correct the `Service` schema

**Files:**
- Modify: `web/src/effect/schema.ts:93-104`

- [ ] **Step 1:** Replace `Service` with a class mirroring `ServiceConfig`:

```ts
export const HealthCheck = Schema.Struct({ path: Schema.String, timeout_seconds: Schema.Number })
export const ResourceLimits = Schema.Struct({ cpu_millis: Schema.Number, memory_bytes: Schema.Number })

export class Service extends Schema.Class<Service>('Service')({
  id: Schema.String,
  project_id: Schema.String,
  name: Schema.String,
  domains: Schema.Array(Schema.String),
  source: ServiceSource,
  internal_port: Schema.Number,
  health_check: HealthCheck,
  resource_limits: Schema.optional(Schema.NullOr(ResourceLimits)),
  env: Schema.Array(Schema.Tuple([Schema.String, Schema.String])),
  tls_enabled: Schema.optionalWith(Schema.Boolean, { default: () => false }),
}) {}
export const Services = Schema.Array(Service)
```

(`ServiceSource` is already defined above in the file. Move the `Service` definition below it if needed.)

- [ ] **Step 2:** Add `ServiceInput` = `Service` fields minus `id` for create. Export it.
- [ ] **Step 3:** `pnpm typecheck` — expect errors in `index.tsx`/`$serviceId.tsx`/`api-client.ts` (number→string, removed fields). Those are fixed in later tasks; do not silence here.
- [ ] **Step 4:** Commit `fix(web): align Service schema with backend ServiceConfig`.

### Task 4: API client — string IDs, getService, deleteService

**Files:**
- Modify: `web/src/effect/api-client.ts` (interface + impl + return object)

- [ ] **Step 1:** Change every service-scoped id param from `number` to `string`: `getServiceDeployments`, `getServiceLogs`, `getServiceMetrics`, `createDeployment.service_id`, `stopService`, `listDomains`, `addDomain`, `verifyDomain`, `deleteDomain`. Keep `listServiceRequests` (already string).
- [ ] **Step 2:** Add to interface + impl: `getService: (id: string) => Effect<Service, ApiError|DecodeError>` (GET `/v1/services/${id}`, `parseResponse(_, Service)`); `deleteService: (id: string) => Effect<void, ApiError>` (DEL, `parseDeleteResponse`). Add both to the returned object.
- [ ] **Step 3:** `putService` keeps signature `(service: Service)` and additionally accept create via `ServiceInput`; simplest: type param as `Service | ServiceInput`. Body is sent as-is (id omitted on create → server assigns).
- [ ] **Step 4:** `pnpm typecheck` for this file's own errors; commit `feat(web): service get/delete client methods, string ids`.

---

## Phase 3: Frontend UI

### Task 5: Tabs primitive

**Files:**
- Create: `web/src/components/Tabs.tsx`
- Test: `web/src/components/Tabs.test.tsx`

- [ ] **Step 1: Failing test.** Render tabs with 3 panels; assert `role="tablist"`, each tab `role="tab"` with `aria-selected`, arrow-key roving focus moves selection, only active panel visible.
- [ ] **Step 2:** Implement a controlled `Tabs` (props: `tabs: {id,label}[]`, `active`, `onChange`, children render-prop or panel map). Mono uppercase labels via `.kicker`, active underline reusing `.nav-link.is-active` pattern. Keyboard: Left/Right/Home/End. No layout-property animation.
- [ ] **Step 3:** Run test. PASS. Commit `feat(web): accessible Tabs component`.

### Task 6: ServiceForm (create/edit)

**Files:**
- Create: `web/src/components/ServiceForm.tsx`
- Test: `web/src/components/ServiceForm.test.tsx`

Responsibilities: controlled form producing a `ServiceInput` (create) or `Service` (edit). Fields: project (select from `listProjects`; locked on edit), name, domains (comma/space split, non-empty), source radio (git | external_image) reusing the existing input groups from `index.tsx:163-245`, internal_port (number >0), health_check.path (defaults `/`), health_check.timeout_seconds (default 5), env (key/value rows, add/remove), resource_limits (optional cpu_millis/memory_bytes), tls_enabled checkbox. Client validation mirrors backend (`domain/service.rs:165-210`): non-empty name, ≥1 domain, port>0, health path starts `/`, timeout>0, image source XOR registry+image_ref.

- [ ] **Step 1: Failing test.** Fill minimal valid external-image service; assert `onSubmit` receives a well-formed `ServiceInput` (no `id`, `source.type==='external_image'`). Assert submit disabled when name empty / no domains / port 0.
- [ ] **Step 2:** Implement using design-system classes (inputs already styled inline in `index.tsx`; lift the pattern). No cards-in-cards; one `.panel` form.
- [ ] **Step 3:** Run test. PASS. Commit `feat(web): ServiceForm for create/edit`.

### Task 7: Services list — create + delete + derived status

**Files:**
- Modify: `web/src/routes/services/index.tsx`
- Modify: `web/src/routes/services/-index.test.tsx`

- [ ] **Step 1:** Replace the "new deployment" panel with a "new service" disclosure that renders `ServiceForm` and calls `putService` (create). On success invalidate `['services']` and close.
- [ ] **Step 2:** Derive status: query `listWorkloads`, build a `Map<service_id, DeploymentStatus | null>`. `WorkloadView.status` is a deployment *phase*, not a string `StatusSignal` expects today, so add an explicit mapping helper: `Healthy → signal-ok`, `Failed → signal-fault`, `Pending|Building|Starting → signal-warn`, `Stopped → signal (neutral)`, `null/absent → render nothing`. Verify `StatusSignal`'s prop type and either feed it the mapped class or extend it to accept `DeploymentStatus | null`. Drop `SecurityBadge` from the row (no list-level source; it already tolerates undefined).
- [ ] **Step 3:** Add a delete action per row (operator only) with inline confirm (reuse the domains delete-confirm pattern from `$serviceId.tsx:605-634`), calling `deleteService`.
- [ ] **Step 4:** Retype all `id` usages to string; `href={/services/${svc.id}}` already string-safe.
- [ ] **Step 5:** Update `-index.test.tsx`: fixtures use string UUIDs and full `ServiceConfig` shape; assert create form submits and delete confirm calls the client.
- [ ] **Step 6:** `pnpm typecheck && pnpm test`. Commit `feat(web): create + delete services in list, derived status`.

### Task 8: Detail page → tabs + Overview/Source + edit/delete

**Files:**
- Modify: `web/src/routes/services/$serviceId.tsx`
- Modify: `web/src/routes/services/-detail.test.tsx`

- [ ] **Step 1:** Change `const id = Number(params.serviceId)` to `const id = params.serviceId` (string). Retype EVERY id-bearing wrapper in this file (`$serviceId.tsx:14-72`): `getDeployments`, `getLogs`, `getMetrics`, `createDeployment`, `stopService`, `listDomains`, `addDomain`, `verifyDomain`, `deleteDomain` all currently take `id: number` / `serviceId: number` and must become `string`. `getRequests` is already string. `Deployment.id` reductions (`a.id > b.id`) stay (deployments keep numeric ids in their own schema — confirm and leave). Replace `services.find(s => s.id === id)` with a `getService(id)` query (endpoint now exists).
- [ ] **Step 2:** Introduce `Tabs` with: Overview (name, project, port, tls, derived status, security posture if present), Source (read-only source summary + Edit button → `ServiceForm` prefilled, calls `putService` with id), Domains (existing `DomainsSection`), Environment (env table from `service.env`), Deployments (existing), Logs (existing), Metrics (existing). Move existing JSX into panels; do not change query logic.
- [ ] **Step 3:** Add header Delete button (operator) with confirm → `deleteService` → navigate to `/services`.
- [ ] **Step 4:** Replace `#{id}` title with `service.name`.
- [ ] **Step 5:** Update `-detail.test.tsx` for string id + tab navigation + getService mock.
- [ ] **Step 6:** `pnpm typecheck && pnpm test`. Commit `feat(web): tabbed service detail with edit/delete`.

---

## Phase 4: Verify

### Task 9: Full verification

- [ ] `cargo build && cargo test && cargo fmt --all` (and `cargo clippy --all-targets` if available) — backend green.
- [ ] `cd web && pnpm typecheck && pnpm test` — frontend green.
- [ ] Manual (per project CLAUDE.md, UI changes need real-app check): `cd web && pnpm build && cargo run`, then with `DENIA_ADMIN_TOKEN` set, create a project, create a service via the form, edit it, deploy/stop, delete it. Confirm list status reflects workloads. Report exact commands + results.
- [ ] Run `gitnexus_detect_changes()` to confirm scope matches expectation before final commit.

---

## Notes for the implementer

- @docs/adr/README.md and ADR-006/008/012 for service/project/RBAC context.
- @web/CLAUDE.md for Effect conventions (no `any`/`as`, `Context.Service`+`Layer`, schema-validated boundary) and the `#/*` path alias.
- Design system: @DESIGN.md. No new colors; signal color only for state; flat panels; mono labels; honor `prefers-reduced-motion`.
- TDD throughout (@superpowers:test-driven-development). Commit per task.
- **Sequencing caveat:** Task 3 (schema rewrite) intentionally breaks `pnpm typecheck` in `index.tsx`/`$serviceId.tsx`/`api-client.ts`. The backend stays green throughout. The frontend typecheck is only expected green again after Task 8. If a per-commit-green tree is required, land Tasks 3+4+7+8 (and their tests) as one squashed "service data-layer + UI" commit instead of four. Either way, the Phase 4 gate is the real green checkpoint.
- Known follow-up (out of scope): a `ServiceView` backend shape that includes runtime status + security posture in one list call, removing the `listWorkloads` join on the client.
