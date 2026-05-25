# Spec: Runtime Security Posture (Frontend) — companion to runtime-security-hardening

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-runtime-security-hardening.md`](2026-05-25-runtime-security-hardening.md)

## Problem

The runtime hardening (user namespace, uid mapping, `no_new_privs`, dropped
capabilities) is invisible in the console. Operators cannot confirm a running
workload is actually sandboxed, which is the whole point of TODO #12.

## Goal

Surface each running workload's security posture in the console as compact,
honest status indicators: is it uid-mapped (not host root), is `no_new_privs`
set, are capabilities dropped. Read-only; no controls.

## Dependency / contract

This needs the backend to **report** posture. The hardening backend plan does not
yet expose it. This spec assumes a small backend addition (tracked as its own
change): `RuntimeStatus` (or the service/metrics payload) gains a
`security` object, e.g.:

```
security: { userns: bool, mapped_uid: u32|null, no_new_privs: bool, caps_dropped: bool }
```

If the field is absent (older backend), the UI shows an "unknown" posture rather
than asserting hardened. **No green-by-default.**

## Decisions

- **Honest signal mapping (DESIGN.md):** all-hardened -> a single steady
  (Stagecraft pink) "sandboxed" indicator; any protection missing -> Breakdown
  violet "weak posture" with which control is missing; unknown -> muted "n/a".
  Never show "secure" without data.
- **Placement:** a `SecurityBadge` on each service row and a posture detail block
  on the service detail view (reuses the console companion's `$serviceId` route).
- **Effect first:** posture comes from the existing service/status `ApiClient`
  query; just extend the Schema. No new endpoint call if it rides on
  `RuntimeStatus`.

## Components / data flow

- Extend the `Service`/status `Schema` with an optional `security` struct.
- `web/src/components/SecurityBadge.tsx` — pure mapping from posture -> signal +
  label + tooltip listing the active protections.
- Mount in `services/index.tsx` rows and `services/$serviceId.tsx` detail.

## Errors / edge cases

- `security` absent -> muted "posture: n/a" (not a fault, not a pass).
- Partial posture (e.g. userns on but caps not dropped) -> violet with the
  specific gap named.
- Stopped workload -> posture not shown (no running process).

## Success criteria

- For a running, hardened workload the operator sees a clear "sandboxed"
  indicator and can expand to see userns/uid/no_new_privs/caps.
- A weak or unknown posture is visually distinct and never masquerades as secure.

## Testing

- `@effect/vitest`: Schema decodes the `security` struct and tolerates its
  absence.
- `@testing-library/react`: `SecurityBadge` mapping — full -> pink/sandboxed,
  partial -> violet/named gap, absent -> muted/n-a.

## Out of scope

The backend hardening itself and the posture-reporting backend change (their own
specs), seccomp/network indicators (not in the hardening pass), any controls to
toggle hardening from the UI.
