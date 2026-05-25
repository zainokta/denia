# Spec: Runtime Security Hardening (TODO #12 / #13)

Status: Draft · Date: 2026-05-25 · Sub-project A of the TODO decomposition

## Problem

Denia runs untrusted workload images under its own Linux runtime. Today
`src/runtime.rs` launches workloads with:

```
unshare --fork --pid --mount --uts --ipc --mount-proc --root <rootfs> --wd <wd> -- <argv>
```

There is **no user namespace, no uid/gid mapping, no capability drop, and no
`no_new_privs`**. The Denia agent runs as host root, so a workload's `root` is
**host root**. A container escape or a malicious image can therefore harm the
host (TODO #12).

TODO #13 ("are we calling system-level Linux or via command?"): isolation is
**command + filesystem**, not raw syscalls. Namespaces come from `unshare(1)`;
cgroup limits are direct cgroup-v2 fs writes (`cpu.max`, `memory.max`,
`cgroup.procs`). No `clone`/`setns`/seccomp syscalls are used.

## Goal

Workloads stop running as host root, run with `no_new_privs`, and run with a
dropped capability bounding set, keeping the existing CLI-tool approach. Seccomp
and network namespace are explicitly out of scope for this pass.

## Decisions

- **Hardening depth:** user namespace + uid/gid map + `no_new_privs` + capability
  drop. No seccomp, no network namespace this pass.
- **Mechanism:** stay CLI. `unshare` user-namespace flags plus a `setpriv`
  wrapper. No move to `nix`/`rustix` syscalls (that is deferred to whenever
  seccomp is added).
- **Cap-drop placement:** because `unshare --root` chroots into the workload
  rootfs, the privilege-dropping tool must exist *inside* the rootfs. Inject a
  static `setpriv` into the rootfs at prepare time and run it as the argv
  wrapper.
- **uid range:** a single node-wide range from config (`base`, `size`); map
  container `0 -> base`. The extracted rootfs bundle is chowned to `base` at
  acquisition. Per-service ranges and idmapped mounts are deferred.

## Target launch command

```
unshare --user \
  --map-users=0,<BASE>,<SIZE> --map-groups=0,<BASE>,<SIZE> \
  --fork --pid --mount --uts --ipc --mount-proc \
  --root <rootfs> --wd <workdir> -- \
  /.denia/setpriv --no-new-privs --bounding-set -all -- <argv...>
```

Container uid/gid 0 maps to host `BASE`; the workload is root *inside* the
namespace but an unprivileged `BASE` on the host. `setpriv` (inside the rootfs)
clears the capability bounding set and sets `no_new_privs` before exec.

## Threat model addressed

- **Closed:** workload running as host uid 0; capability-based escapes; privilege
  re-gain via setuid binaries (`no_new_privs`).
- **Not closed this pass:** syscall-surface attacks (no seccomp); host network
  access (no network namespace, ingress still via Denia sockets); cross-workload
  host-uid isolation (single shared range).

## Constraints / risks

- **Static setpriv required.** A dynamically linked `setpriv` fails inside a
  scratch rootfs. The configured binary must be a statically linked build.
- **Shared-bundle mutation.** Injecting `setpriv` and chowning happen on a
  digest-shared bundle; idempotent, but concurrent first-deploys of the same
  digest could race. Acceptable for now.
- Requires kernel user-namespace support. Running as root usually bypasses
  `kernel.unprivileged_userns_clone=0`, but a kernel can still disable or omit
  user namespaces.

## Success criteria

- A deployed workload's host PID runs as uid `BASE`, not 0.
- `/proc/<workload>/status` shows `NoNewPrivs: 1` and a cleared `CapBnd`.
- Existing API/runtime behaviour and tests unchanged otherwise.

## Out of scope

Seccomp syscall filtering, network namespace + bridge rework, per-service uid
ranges, idmapped mounts. The other 7 TODO sub-projects (Projects, RBAC,
Observability, Ingress/TLS, Jobs, Installer, Analytics).
