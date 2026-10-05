# ADR-040: Operator-Requested Prune (`denia clean`)

- **Status**: Proposed
- **Date**: 2026-10-05

## Context

Several Denia stores grow on disk with no bound:

- **Rootfs bundles** (`<artifact_dir>/<digest>/`) are written by
  `ArtifactAcquirer::write_bundle` and never deleted. One survives per image
  digest ever deployed.
- **BuildKit cache** (`/var/lib/buildkit`, provisioned by `denia setup`) is
  never pruned.
- **Logs** (`<log_dir>/{service_id}.log`, `<log_dir>/deployments/<id>.log`)
  are append-only and only wiped by `clean_session_logs` on daemon start.
- **Crash leftovers** (`<artifact_dir>/builds/<uuid>`,
  `<artifact_dir>/git-checkouts/<uuid>`, `<uploads_dir>/<id>`,
  `<artifact_dir>/<digest>/rootfs.*.tmp`) are removed on normal paths but
  survive an interrupted run.

The OCI layer cache (ADR-022) already has periodic GC with a manual endpoint.
Hosted registry storage is owned by the managed Zot instance (ADR-031), which
runs its own GC. Operators have no single command to reclaim space now.

## Decision

1. Add a super-admin endpoint pair under `/v1/system/prune`:
   - `GET` returns a **plan**: reclaimable bytes and entry counts per category,
     computed without deleting anything.
   - `POST` executes the prune and returns the same shape with what was
     actually deleted.
   The work runs in the daemon because only the daemon holds the reservations
   that stop it from deleting data an in-flight deploy, autoscale, or pull is
   using.

2. Add `denia clean` as a thin `/v1` client (profile token, which must be
   super-admin). It prints
   the plan, prompts y/N, and then calls `POST`. `--yes` skips the prompt;
   `--dry-run` prints the plan and exits.

3. Categories pruned:
   - **Rootfs bundles** whose digest is not the promoted artifact of any
     service (kept for `Redeploy`, ADR-039), not linked to a `Pending`,
     `Building`, or `Starting` deployment, and not used by a running replica.
     Bundles touched within a one-hour grace window are also kept; this
     covers the gap between `write_bundle` and `set_deployment_artifact`, and
     one-shot jobs, without adding a lock to the deploy pipeline.
   - **BuildKit cache** via `buildctl prune`.
   - **OCI layer cache**: run the existing ADR-022 sweep (same retention and
     guards). Retention is not overridden, so layers for a pruned bundle stay
     available for a fast rebuild.
   - **Logs**: delete closed deployment logs and logs of services that no
     longer exist; truncate live service logs to zero. Unlinking a live log
     would not free space until the workload exits.
   - **Crash leftovers** not owned by an in-flight build or upload.

4. Out of scope: journald, host package caches, and hosted registry storage
   (Zot GC owns it; Denia never deletes tags).

5. Naming: **GC** stays the automatic, periodic, single-store sweep. **Prune**
   is the operator-requested, multi-store reclamation. The CLI verb is
   `clean`.

## Consequences

- Easier: one previewed, confirmed command reclaims space across every Denia
  store, locally or remotely.
- Harder: bundle safety rests on the keep-set plus the grace window, not on a
  lock. A deploy whose acquire-to-link gap exceeds the grace window could
  lose its bundle; a lock across the deploy pipeline is the upgrade path.
- A pruned bundle for a re-deployed image is rebuilt from the OCI cache or
  registry: slower, but correct.

## Alternatives Considered

- **Host-local `sudo denia clean`** walking the filesystem: rejected; it races
  with deploys and autoscale and cannot see daemon reservations.
- **Periodic bundle GC instead of an operator command**: deferred; the
  operator command gives a preview first, and a timer can call the same
  planner later.
- **Artifact RwLock held by deploy/job paths**: deferred; more invasive
  (coordinator, scheduler, autoscale) for a gap the grace window covers.
- **Deleting old registry tags**: rejected; breaks pulls that previously
  worked.
