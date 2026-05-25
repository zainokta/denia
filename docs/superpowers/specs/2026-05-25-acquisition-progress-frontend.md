# Spec: Build / Acquisition Progress (Frontend) — companion to inprocess-oci-acquisition

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-inprocess-oci-acquisition.md`](2026-05-25-inprocess-oci-acquisition.md)

## Problem

Deploying a service runs an acquisition pipeline (build via BuildKit, or pull +
unpack an OCI image into a rootfs bundle). In the console a deploy currently
looks like an opaque status flip. Operators cannot see which phase a deploy is in
or which artifact resulted.

## Goal

Make a deploy legible: show the acquisition/build phase progression during a
deployment, and display the resulting artifact digest/kind once acquired.
Read-only, woven into the deployment views from the operator-console companion.

## Dependency / contract

`DeploymentStatus` already has `Pending / Building / Starting / Healthy / Failed /
Stopped`. This frontend maps those to phase labels. Showing the **artifact
digest** needs the deployment (or a related endpoint) to expose it; if the
backend does not yet return an artifact reference on the deployment, the digest
section degrades to "pending" and is tracked as a small backend addition. No
fabricated values.

## Decisions

- **Phase model from existing status** (no new stream in v1):
  `Pending`->queued, `Building`->acquiring (build/pull+unpack), `Starting`->
  starting runtime, `Healthy`->live, `Failed`->fault, `Stopped`->stopped.
- **Signal mapping (DESIGN.md):** in-progress phases -> warn; `Healthy` -> ok;
  `Failed` -> Breakdown violet; the active deploy's primary affordance stays pink.
- **Polling, not streaming:** reuse the deployments Query; while a deployment is
  in a non-terminal phase, poll faster (e.g. 2s) and stop on terminal.
- **Artifact display:** when present, show digest (mono, truncated with copy) and
  kind (OciImage / RootfsBundle).

## Components / data flow

- A `DeployPhase` component: maps `DeploymentStatus` -> ordered phase steps with
  the current one highlighted (a compact stepline, not a spinner farm).
- Optional `ArtifactRef` schema + display when the deployment carries it.
- Mounted in the operator-console `services/$serviceId` deployments timeline; the
  active (newest non-terminal) deployment shows the phase line.

## Errors / edge cases

- `Failed` -> phase line stops at the failed phase in violet; surface the error
  message if the backend provides one.
- No artifact ref yet -> "artifact: pending".
- Terminal deployment -> static phase summary, polling stops.

## Success criteria

- During a deploy the operator sees it move queued -> acquiring -> starting ->
  live, with a clear fault state on failure.
- Once acquired, the artifact digest + kind are visible on the deployment.

## Testing

- `@effect/vitest`: optional `ArtifactRef` schema decodes / tolerates absence.
- `@testing-library/react`: `DeployPhase` highlights the right step per status;
  `Failed` renders the violet fault stop; artifact digest renders when present.

## Out of scope

Real-time build log streaming (the console logs panel covers post-start logs),
per-phase timing/metrics, retry controls. The acquisition pipeline itself (its
own plan) and any backend artifact-reference addition.
