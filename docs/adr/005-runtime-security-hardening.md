# ADR-005: Runtime Security Hardening

## Status

Proposed

## Date

2026-05-25

## Context

Denia runs untrusted workload images under its own Linux runtime with PID, mount, UTS, and
IPC namespaces. Services and one-shot jobs now use Denia's direct fork/unshare/exec syscall
adapter instead of a host namespace launcher.

A container escape or malicious image can therefore harm the host (TODO #12). TODO #13
confirmed isolation is command + filesystem, not raw syscalls, so the CLI approach should be
extended rather than replaced.

## Decision

Workload isolation is hardened through three runtime additions:

1. **User namespace + uid/gid mapping.** Denia's native unshare adapter maps container uid/gid 0
   to a configurable host base uid. A single node-wide range
   (`DENIA_USERNS_BASE`, default `100000`; `DENIA_USERNS_SIZE`, default `65536`) is used.
   Container root is an unprivileged host uid.

2. **no_new_privs + capability drop.** Denia injects its own stage-1 helper into the
   rootfs, then the helper calls `rustix::thread::set_no_new_privs(true)` and
   `rustix::thread::remove_capability_from_bounding_set` for every supported capability before
   spawning the workload. Services use `/.denia/socket-proxy`; jobs use
   `/.denia/workload-launcher`. Denia rejects symlinked rootfs directories, symlinked
   `/.denia` directories, and symlinked helper targets before injecting helpers, so untrusted
   image contents cannot redirect these host-root writes outside the rootfs.

3. **Rootfs ownership.** After OCI image unpacking, Denia calls
   `syscall::chown::recursive_lchown` on the rootfs directory, matching the uid the container root
   maps to without following symlinks.

`src/syscall/ns.rs` validates argv/env/rootfs/workdir before fork, writes uid/gid maps from the
parent, attaches the child to cgroup v2, mounts proc, applies `no_new_privs`, drops the capability
bounding set, and execs the injected helper. Services exec `/.denia/socket-proxy`, which owns
Denia's Unix socket contract, brings the namespace loopback interface up, and bridges it to the
workload's configured internal TCP port inside the namespace. Jobs exec
`/.denia/workload-launcher`. Production hosts can provide the injected proxy through
`DENIA_SOCKET_PROXY_BINARY`; the default is the current Denia executable.
Seccomp and broader network device/DNS setup inside the workload network namespace are explicitly
deferred.

## Consequences

### Positive

- Workloads no longer run as host root. Container uid 0 maps to an unprivileged host uid.
- `no_new_privs` prevents privilege escalation via setuid binaries inside the rootfs.
- Cleared capability bounding set blocks capability-based escapes.
- Rootfs files are owned by the mapped host uid, making permission boundaries meaningful.
- Runtime preparation no longer follows rootfs or `/.denia` helper symlinks while mutating the
  bundle as host root.
- Workloads no longer share the host network namespace or host mount propagation state.
- Workloads can keep their normal TCP bind behavior while ingress still reaches them through a
  Denia-owned Unix socket path.
- Loopback setup does not depend on `iproute2` being present in the workload rootfs.
- User namespace ranges and host proxy binary are configurable via `DENIA_USERNS_BASE`,
  `DENIA_USERNS_SIZE`, and `DENIA_SOCKET_PROXY_BINARY` environment variables.
- `setpriv` is no longer a host dependency; `no_new_privs` and capability bounding-set drops are
  applied through `src/syscall/caps.rs`.

### Negative

- The injected Denia socket-proxy binary must also be usable inside the rootfs; production
  packaging should prefer a static Denia binary for scratch/distroless images.
- Shared-bundle mutation (chown + helper injection) on digest-shared bundles could race on
  concurrent first-deploys of the same digest. Symlink redirection is rejected, but concurrent
  mutation of the same safe bundle remains acceptable for the current single-node control plane.
- Host kernel must have user namespace support enabled. Running the Denia agent as root
  usually bypasses unprivileged-userns sysctl policy, but not kernels that disable or omit
  user namespaces entirely.
- The first network namespace pass configures loopback for workload ingress, but does not yet
  configure veth links or DNS policy inside that namespace.
- Raw Linux calls that require Rust `unsafe` are isolated in syscall adapter modules instead of
  being spread through the runtime.

## Alternatives Considered

- **Move to `nix`/`rustix` syscalls**: Deferred. The CLI approach keeps the surface small and
  auditable. A syscall refactor is better paired with seccomp work in a future pass.
- **Per-service uid ranges**: Deferred. A single shared range is sufficient for the current
  single-node control plane. Per-service isolation via idmapped mounts or separate ranges
  can be added when multi-tenancy isolation requirements grow.
- **Drop capabilities via namespace flags**: Rejected. Linux namespace flags do not provide
  `no_new_privs` or capability bounding set control.
- **Use `setpriv` or `capsh` inside the rootfs**: Rejected. Denia now owns this hardening through
  rustix syscalls, avoiding extra host binaries and scratch-rootfs dynamic-linker failures.

## References

- [ADR-003: Linux Runtime Process Runner](003-linux-runtime-process-runner.md)
- `src/config.rs`, `src/runtime.rs`, `src/syscall/ns.rs`, `src/artifacts/acquirer.rs`, `src/app.rs`
- `tests/linux_runtime_privileged.rs`
- `docs/superpowers/specs/2026-05-25-runtime-security-hardening.md`
