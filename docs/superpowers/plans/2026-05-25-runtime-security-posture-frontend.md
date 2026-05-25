# Runtime Security Posture (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface each running workload's security posture (userns/uid-mapped, no_new_privs, caps dropped) in the console as an honest, read-only signal.

**Architecture:** Extend the service/status Schema with an optional `security` struct, then render a `SecurityBadge` (pure posture -> DESIGN.md signal mapping) on service rows and the detail view. No new endpoint if posture rides on the existing status payload.

**Tech Stack:** TanStack Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-runtime-security-posture-frontend.md`. Depends on a backend posture-reporting field.

---

## File Structure

- `web/src/effect/schema.ts` — add optional `security` to the service/status schema.
- `web/src/components/SecurityBadge.tsx` — posture -> signal mapping.
- `web/src/routes/services/index.tsx` + `$serviceId.tsx` — mount the badge.
- Tests colocated.

Commit after each task.

---

## Task 1: Schema — optional `security`

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — decode a payload **with** `security` (all true) and **without** it (field optional/undefined).

```ts
it.effect('decodes security posture when present', () => /* assert fields */)
it.effect('tolerates missing security posture', () => /* assert undefined, no error */)
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement**

```ts
export const SecurityPosture = Schema.Struct({
  userns: Schema.Boolean,
  mapped_uid: Schema.NullOr(Schema.Number),
  no_new_privs: Schema.Boolean,
  caps_dropped: Schema.Boolean,
})
// add to the service/status class:
//   security: Schema.optional(SecurityPosture)
```

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): security posture schema"`

---

## Task 2: SecurityBadge component

**Files:**
- Create: `web/src/components/SecurityBadge.tsx`
- Test: `web/src/components/SecurityBadge.test.tsx`

- [ ] **Step 1: Write failing tests**
  - full posture (all true) -> `signal-steady` + "sandboxed".
  - partial (e.g. `caps_dropped: false`) -> `signal-fault` + names the gap.
  - `undefined` -> muted + "posture: n/a".
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — pure function component taking `security?: SecurityPosture`; compute `hardened = userns && no_new_privs && caps_dropped`; render the DESIGN.md `.signal-*` class + label + a tooltip/`title` listing active protections. No color literals. Never "secure" without data.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): SecurityBadge posture indicator"`

---

## Task 3: Mount in services list + detail

**Files:**
- Modify: `web/src/routes/services/index.tsx`, `web/src/routes/services/$serviceId.tsx`
- Test: extend the existing route tests

- [ ] **Step 1: Write failing test** — a service row with full posture shows the sandboxed badge; the detail view shows the expanded posture block.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — render `<SecurityBadge security={service.security} />` in each row; on the detail view add a posture `.panel` listing userns/mapped uid/no_new_privs/caps. Only for running workloads.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): show security posture in console"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck`.
- [ ] `cd web && pnpm test` — schema tolerance + badge mapping green.
- [ ] `pnpm build`.
- [ ] Manual (backend reporting posture): a hardened workload shows "sandboxed";
  simulate a missing control -> violet gap; older backend (no field) -> n/a.

## Notes

- Depends on the backend exposing a `security` field (its own change). Until then
  the UI degrades to "n/a" — that is the correct, honest behaviour.
- Builds on the operator-console companion (`services/*` routes); sequence after it.
- Honor DESIGN.md: never green-by-default; pink = sandboxed, violet = weak gap.
