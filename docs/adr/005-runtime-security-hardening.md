# ADR-005: Runtime Security Hardening

## Status

Proposed

## Date

2026-05-25

## Context

Denia runs untrusted workload images under its own Linux runtime using `unshare` with PID,
mount, UTS, and IPC namespaces. However, workloads currently run with `root` in the container
mapping to host `uid 0`, no `no_new_privs` flag, and a full capability bounding set.

A container escape or malicious image can therefore harm the host (TODO #12). TODO #13
confirmed isolation is command + filesystem, not raw syscalls, so the CLI approach should be
extended rather than replaced.

## Decision

Workload isolation is hardened through three CLI-level additions:

1. **User namespace + uid/gid mapping.** `unshare --user` with `--map-users` and `--map-groups`
   maps container uid/gid 0 to a configurable host base uid. A single node-wide range
   (`DENIA_USERNS_BASE`, default `100000`; `DENIA_USERNS_SIZE`, default `65536`) is used.
   Container root is an unprivileged host uid.

2. **no_new_privs + capability drop.** A statically linked `setpriv` binary is injected into
   the rootfs at `/.denia/setpriv` during `prepare()`. `setpriv --no-new-privs --bounding-set -all`
   is prepended to the workload argv inside the namespace, preventing privilege re-gain via
   setuid binaries and clearing the capability bounding set. Denia rejects symlinked rootfs
   directories, symlinked `/.denia` directories, and symlinked `/.denia/setpriv` targets before
   injecting the binary, so untrusted image contents cannot redirect this host-root write outside
   the rootfs.

3. **Rootfs ownership.** After OCI image unpacking, `chown -R --no-dereference <base>:<base>`
   is run on the rootfs directory, matching the uid the container root maps to.

Runtime first starts Denia in host-side `__denia_cgroup_launcher` mode. That launcher writes its
own PID to `cgroup.procs`, writes a ready marker, then `exec`s the namespace command. The resulting
namespace launch command is wrapped by Denia's namespace-local socket proxy:

```
unshare --user \
  --map-users=0,<BASE>,<SIZE> --map-groups=0,<BASE>,<SIZE> \
  --fork --pid --net --mount --propagation private --uts --ipc --mount-proc \
  --root <rootfs> --wd <workdir> -- \
  /.denia/socket-proxy --listen /run/denia/service.sock --connect 127.0.0.1:<PORT> -- \
  /.denia/setpriv --no-new-privs --bounding-set -all -- <argv...>
```

The mechanism stays CLI (`unshare` + injected `setpriv`) rather than moving to `nix`/`rustix`
syscalls. Denia also requests a separate network namespace and private mount propagation in the
same `unshare` invocation. The injected socket proxy owns Denia's Unix socket contract, brings the
namespace loopback interface up, and bridges it to the workload's configured internal TCP port
inside the namespace. Seccomp and broader network device/DNS setup inside the workload network
namespace are explicitly deferred.

## Consequences

### Positive

- Workloads no longer run as host root. Container uid 0 maps to an unprivileged host uid.
- `no_new_privs` prevents privilege escalation via setuid binaries inside the rootfs.
- Cleared capability bounding set blocks capability-based escapes.
- Rootfs files are owned by the mapped host uid, making permission boundaries meaningful.
- Runtime preparation no longer follows rootfs or `/.denia/setpriv` symlinks while mutating the
  bundle as host root.
- Workloads no longer share the host network namespace or host mount propagation state.
- Workloads can keep their normal TCP bind behavior while ingress still reaches them through a
  Denia-owned Unix socket path.
- Loopback setup does not depend on `iproute2` being present in the workload rootfs.
- All three hardening dimensions are configurable via `DENIA_USERNS_BASE`, `DENIA_USERNS_SIZE`,
  and `DENIA_SETPRIV_BINARY` environment variables.
- Existing CLI tool approach is preserved; no new syscall dependencies.

### Negative

- A statically linked `setpriv` binary must be available on the host. A dynamically linked
  binary would fail inside a scratch rootfs with no shared libraries.
- The injected Denia socket-proxy binary must also be usable inside the rootfs; production
  packaging should prefer a static Denia binary for scratch/distroless images.
- Shared-bundle mutation (chown + setpriv injection) on digest-shared bundles could race on
  concurrent first-deploys of the same digest. Symlink redirection is rejected, but concurrent
  mutation of the same safe bundle remains acceptable for the current single-node control plane.
- Host kernel must have user namespace support enabled. Running the Denia agent as root
  usually bypasses unprivileged-userns sysctl policy, but not kernels that disable or omit
  user namespaces entirely.
- The first network namespace pass configures loopback for workload ingress, but does not yet
  configure veth links or DNS policy inside that namespace.

## Alternatives Considered

- **Move to `nix`/`rustix` syscalls**: Deferred. The CLI approach keeps the surface small and
  auditable. A syscall refactor is better paired with seccomp work in a future pass.
- **Per-service uid ranges**: Deferred. A single shared range is sufficient for the current
  single-node control plane. Per-service isolation via idmapped mounts or separate ranges
  can be added when multi-tenancy isolation requirements grow.
- **Drop capabilities via `unshare` flags**: Rejected. `unshare` does not provide
  `no_new_privs` or capability bounding set control. `setpriv` is the standard utility-land
  tool for this.
- **Use `capsh` instead of `setpriv`**: Rejected. `capsh` is part of `libcap2-bin`, which is
  less commonly statically linked and has a different interface.

## References

- [ADR-003: Linux Runtime Process Runner](003-linux-runtime-process-runner.md)
- `src/config.rs`, `src/runtime.rs`, `src/artifacts/acquirer.rs`, `src/app.rs`
- `tests/linux_runtime_privileged.rs`
- `docs/superpowers/specs/2026-05-25-runtime-security-hardening.md`
