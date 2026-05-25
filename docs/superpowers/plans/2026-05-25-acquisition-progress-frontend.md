# Build / Acquisition Progress (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a deploy legible in the console: a phase stepline driven by `DeploymentStatus`, faster polling while in progress, and the resulting artifact digest/kind once available.

**Architecture:** A pure `DeployPhase` component maps `DeploymentStatus` to ordered phase steps with DESIGN.md signal colors. The deployments Query polls faster while the newest deployment is non-terminal. An optional `ArtifactRef` schema renders the digest when the backend provides it.

**Tech Stack:** TanStack Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-acquisition-progress-frontend.md`. Builds on the operator-console companion.

---

## File Structure

- `web/src/components/DeployPhase.tsx` — status -> phase stepline.
- `web/src/effect/schema.ts` — optional `ArtifactRef` (digest, kind) on deployment.
- `web/src/routes/services/$serviceId.tsx` — mount phase line + artifact display; faster poll while in progress.
- Tests colocated.

Commit after each task.

---

## Task 1: DeployPhase component

**Files:**
- Create: `web/src/components/DeployPhase.tsx`
- Test: `web/src/components/DeployPhase.test.tsx`

- [ ] **Step 1: Write failing tests**
  - `Building` -> "acquiring" step active, earlier steps done, later steps idle, warn signal.
  - `Healthy` -> all steps done, ok signal.
  - `Failed` -> stops at the failed step, `signal-fault` (violet).
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — ordered steps `["queued","acquiring","starting","live"]`; map status -> active index (`Pending`=0, `Building`=1, `Starting`=2, `Healthy`=3 all done, `Failed`=stop at last reached + fault, `Stopped`=muted summary). Render a compact stepline using `.kicker` labels + `.signal-*` dots. Pure, no data fetching.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): DeployPhase stepline"`

---

## Task 2: Optional ArtifactRef schema

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `Deployment` decodes **with** an `artifact` ref (digest, kind) and **without** it.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement**

```ts
export const ArtifactRef = Schema.Struct({
  digest: Schema.String,
  kind: Schema.Literal('OciImage', 'RootfsBundle'),
})
// add to Deployment class:  artifact: Schema.optional(ArtifactRef)
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): optional artifact ref on deployment"`

---

## Task 3: Mount in service detail + in-progress polling

**Files:**
- Modify: `web/src/routes/services/$serviceId.tsx`
- Test: `web/src/routes/services/$serviceId.test.tsx`

- [ ] **Step 1: Write failing test**
  - the newest non-terminal deployment renders `DeployPhase` at the right step.
  - when present, the artifact digest (mono, truncated) + kind render.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement**
  - Compute the newest deployment; if its status is non-terminal
    (`Pending|Building|Starting`), set the deployments Query `refetchInterval` to
    ~2s, else disable polling.
  - Render `<DeployPhase status={newest.status} />` above the timeline.
  - If `newest.artifact` present, show digest (truncated mono + copy button) and
    kind; else "artifact: pending".
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): deploy phase + artifact in service detail"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck`.
- [ ] `cd web && pnpm test` — phase mapping, schema tolerance, detail wiring green.
- [ ] `pnpm build`.
- [ ] Manual (backend running): trigger a deploy; watch queued -> acquiring ->
  starting -> live; force a failure -> violet fault stop; confirm digest shows
  once acquired (or "pending" if the backend omits it).

## Notes

- Builds on the operator-console companion (`services/$serviceId`); sequence after it.
- Artifact digest depends on the backend returning an artifact ref on the
  deployment; until then the UI shows "pending" (no fabricated values).
- Honor DESIGN.md: in-progress = warn, live = ok, failure = Breakdown violet.
