# Spec: Runtime Security Hardening (TODO #12 / #13)

Status: Done · Date: 2026-05-25 · Sub-project A of the TODO decomposition

> **Implementation note:** The implementation diverged from the original CLI/setpriv
> approach and instead uses Denia's native fork/unshare/exec syscall adapter
> (`src/syscall/ns.rs`) with in-process `rustix` syscalls for `no_new_privs` and
> capability bounding-set drop (`src/syscall/caps.rs`). See ADR-005 for rationale.

## Problem

Denia runs untrusted workload images under its own Linux runtime. Previously
`src/runtime.rs` launched workloads via `unshare(1)` with PID, mount, UTS, and
IPC namespaces.

There was **no user namespace, no uid/gid mapping, no capability drop, and no
`no_new_privs`**. The Denia agent runs as host root, so a workload's `root` was
**host root**. A container escape or a malicious image could therefore harm the
host (TODO #12).

## Goal

Workloads stop running as host root, run with `no_new_privs`, and run with a
dropped capability bounding set. Seccomp and network namespace are explicitly
out of scope for this pass.

## Decisions

- **Hardening depth:** user namespace + uid/gid map + `no_new_privs` + capability
  drop. No seccomp, no network namespace this pass.
- **Mechanism:** direct syscall adapter. Denia's native fork/unshare/exec adapter
  (`src/syscall/ns.rs`) sets up user namespaces, writes uid/gid maps from the
  parent, attaches to cgroup v2, mounts proc, and drops privileges in-process
  via `rustix::thread` syscalls (`src/syscall/caps.rs`). `setpriv` is no longer
  a host dependency.
- **Cap-drop placement:** `no_new_privs` and capability bounding-set drop are
  applied in the child process after namespace setup and before exec. Services
  defer hardening to the socket-proxy helper (which needs capabilities to bring
  up loopback), then the proxy drops capabilities before running the workload.
  Jobs apply immediate hardening.
- **uid range:** a single node-wide range from config (`base`, `size`); map
  container `0 -> base`. The extracted rootfs bundle is chowned to `base` at
  acquisition via `syscall::chown::recursive_lchown`. Per-service ranges and
  idmapped mounts are deferred.

## Namespace configuration

Denia uses a typed `NamespaceConfig` (in `src/syscall/ns.rs`) with fields:
- `userns: bool` (default `true`)
- `uid_map`, `gid_map`: container-to-host mapping
- `pid_ns`, `mount_ns`, `uts_ns`, `ipc_ns`, `net_ns`: all default `true`
- `mount_proc: bool` (default `true`)
- `no_new_privs: bool` (default `true`)
- `drop_bounding_caps: bool` (default `true`)

The `spawn_namespaced_process()` function handles fork, unshare with all
configured namespace flags, uid/gid map writing via `/proc/<pid>/uid_map` and
`/proc/<pid>/gid_map` (with `setgroups deny`), cgroup attachment, proc mount,
`no_new_privs` + capability bounding-set drop, and `execve`.

## Threat model addressed

- **Closed:** workload running as host uid 0; capability-based escapes; privilege
  re-gain via setuid binaries (`no_new_privs`).
- **Not closed this pass:** syscall-surface attacks (no seccomp); host network
  access (no network namespace, ingress still via Denia sockets); cross-workload
  host-uid isolation (single shared range).

## Constraints / risks

- The injected Denia socket-proxy binary must be usable inside the rootfs;
  production packaging should prefer a static Denia binary for scratch/distroless
  images.
- Shared-bundle mutation (chown + helper injection) on digest-shared bundles
  could race on concurrent first-deploys of the same digest. Acceptable for
  current single-node control plane.
- Requires kernel user-namespace support. Running as root usually bypasses
  `kernel.unprivileged_userns_clone=0`, but a kernel can still disable or omit
  user namespaces.
- Raw Linux calls requiring `unsafe` are isolated in `src/syscall/` modules.

## Success criteria

- A deployed workload's host PID runs as uid `BASE`, not 0.
- `/proc/<workload>/status` shows `NoNewPrivs: 1` and a cleared `CapBnd`.
- Existing API/runtime behaviour and tests unchanged otherwise.

## Out of scope

Seccomp syscall filtering, network namespace + bridge rework, per-service uid
ranges, idmapped mounts. The other 7 TODO sub-projects (Projects, RBAC,
Observability, Ingress/TLS, Jobs, Installer, Analytics).
