# ADR-019: Per-Replica Runtime Filesystem Isolation

- **Status**: Accepted
- **Date**: 2026-05-27

## Context

ADR-018 introduces autoscaling: a service may run multiple replicas of the same
deployment. The current Linux runtime (ADR-003) launches a workload by
`pivot_root`-ing directly into the content-addressed artifact rootfs
(`artifact_dir/<digest>/rootfs`) and creating its ingress socket at
`/run/denia/service.sock` *inside that rootfs*. It also copies the runtime
helper binaries (socket-proxy, workload-launcher) into `rootfs/.denia/`.

This rootfs is shared by every replica of a deployment (it is content-addressed
and reused). Running more than one replica against it is unsafe:

- Two replicas bind the same host-visible socket path → collision.
- The child-process map is keyed by `service_name`, so a second replica evicts
  the first.
- Any workload that writes to its own filesystem (logs, caches, temp files,
  embedded databases, uploads — normal for arbitrary PaaS images) has replicas
  clobbering each other.
- The runtime mutates the content-addressed bundle at launch, which is already a
  latent bug for single instances and becomes a correctness problem with N
  replicas.

## Decision

Every workload — including a single instance, treated as replica index 0 —
launches into a **private per-replica overlay filesystem**. There is one unified
launch path; the previous direct-rootfs path is removed.

- **Overlay layout** (mounted in the workload's mount namespace before
  `pivot_root`, at the existing rootfs self-bind point in `src/syscall/ns.rs`):
  - `lowerdir` = the shared artifact rootfs, treated as **read-only**.
  - `upperdir` + `workdir` = a per-replica writable layer at
    `runtime_dir/{service_id}/{deployment_id}/{replica_index}/{upper,work}`.
  - the merged mount becomes the namespace root.
- Because each replica's rootfs view is private and writable, `/run/denia` and
  the ingress socket are automatically private per replica. No separate socket
  bind-mount is required, and the content-addressed bundle is never mutated.
- **Helper binaries** (socket-proxy, workload-launcher) are **bind-mounted
  read-only** into the guest (e.g. `/.denia/socket-proxy`) instead of being
  copied into the rootfs, since the lower is read-only and the upper is
  ephemeral.
- The per-replica upper/work layer is **ephemeral**: created on launch, removed
  on drain/stop. Replicas are stateless cattle, consistent with autoscaling and
  scale-to-zero. Boot reconcile removes orphaned upper directories.
- `NamespaceConfig` (`src/syscall/ns.rs`) gains support for an overlay mount
  spec and an ordered list of read-only bind mounts, both applied before
  `pivot_root`.

## Consequences

- Replicas are fully filesystem-isolated; arbitrary PaaS workloads that write to
  disk are safe to scale horizontally.
- The single-instance bundle-mutation bug is fixed as a side effect of unifying
  on the overlay path.
- The privileged runtime gains overlay + bind-mount syscalls. This is the most
  delicate change in the autoscaling work and is covered by a dedicated, gated
  privileged test (`DENIA_RUN_PRIVILEGED_TESTS=1`).
- Disk usage grows by the per-replica writable delta (upper layers), not by full
  rootfs copies; ephemeral cleanup bounds it.
- Slightly higher launch cost (overlay mount setup) per workload.

## Risks

- **Unprivileged-userns overlay support**: mounting overlayfs inside a user
  namespace requires Linux ≥ 5.11. Denia's target hosts run modern kernels; the
  gated privileged test asserts the overlay mounts succeed, and the runtime
  returns a typed error (rather than panicking) if the kernel rejects the mount.

## Alternatives Considered

- **Minimal: private `/run/denia` bind-mount only**, leaving the rest of the
  rootfs shared. Smallest change and fixes the socket collision, but leaves
  replicas clobbering other writable paths and keeps mutating the shared bundle.
  Rejected — does not make arbitrary workloads safe to replicate.
- **Per-replica full rootfs copy**. Simple semantics but heavy disk use and slow
  launches; defeats content-addressed sharing. Rejected.
- **Bake helper binaries into the artifact at build time** (so they live in the
  read-only lower). Avoids per-launch bind mounts but changes the build path and
  bloats every stored artifact. Rejected in favour of read-only bind mounts.

## References

- ADR-003 (Linux Runtime Process Runner) — the runtime this extends
- ADR-018 (Autoscaling) — the feature requiring per-replica isolation
- `docs/superpowers/specs/2026-05-27-autoscaling-design.md`
- `src/syscall/ns.rs` — namespace/mount setup
