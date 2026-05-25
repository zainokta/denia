# ADR-003: Linux Runtime Process Runner

## Status

Accepted

## Date

2026-05-24

## Context

ADR-001 accepted a Denia-owned Linux runtime, but the first implementation only defined the runtime trait and an ignored privileged test gate. Denia now needs the first concrete low-level runner path without introducing Docker, containerd, or runc as the service runtime.

The runner must keep host root as the trust boundary, place workloads into Denia-owned cgroups, and start processes through Linux namespace primitives. Normal CI must remain unprivileged; namespace/cgroup execution tests must stay opt-in.

## Decision

`LinuxRuntime` owns the first process-runner contract:

- Runtime input must reference a `RootfsBundle` artifact.
- Rootfs bundles live under `artifact_dir/<safe-digest>/rootfs`.
- Each bundle has `process.json`, containing `argv`, `env`, and `workdir`.
- External images are copied into the local OCI layout with `skopeo`, then unpacked into the rootfs bundle directory with `umoci unpack`.
- Git/Dockerfile builds export OCI layout output from BuildKit, then use the same `umoci unpack` rootfs bundle path.
- Rootfs bundle ownership is adjusted to the configured host user-namespace base uid/gid so container uid/gid 0 maps to non-host-root ownership.
- `process.json` is derived from OCI image config `Entrypoint`, `Cmd`, `Env`, and `WorkingDir` when Denia materializes an external image bundle.
- The deployment coordinator has external-image and Git paths that materialize a rootfs bundle before calling `Runtime::start`.
- The `/v1/deployments` API uses the acquisition path for external-image and Git deployment requests.
- `POST /v1/services/{service_id}/stop` calls the runtime stop path for the stored service, marks the promoted deployment `Stopped`, and clears the promotion pointer.
- `GET /v1/services/{service_id}/logs` reads recent log lines from Denia's per-service log files under `log_dir`.
- `LinuxRuntime` redirects workload stdout and stderr into Denia's per-service log file under `log_dir`.
- `GET /v1/services/{service_id}/metrics` reads the promoted deployment's cgroup v2 `cpu.stat` and `memory.current` files from the configured `cgroup_root`.
- Denia has a `LoopbackBridge` primitive that accepts loopback TCP connections and forwards bytes to a Denia-owned Unix socket.
- Denia supervises a per-service loopback bridge task on promotion, replaces it when the same service is promoted again, and deactivates it when the service is stopped.
- `LinuxRuntime` owns the workload Unix socket contract by injecting `DENIA_SERVICE_SOCKET=/run/denia/service.sock` into the child environment.
- The host-visible bridge target for that socket is `<rootfs>/run/denia/service.sock`, which corresponds to `/run/denia/service.sock` after the workload is launched with `--root <rootfs>`.
- `LinuxRuntime` injects the Denia binary into the rootfs as `/.denia/socket-proxy` and uses it as the namespace stage-1 process.
- The stage-1 socket proxy brings the namespace loopback interface up, binds `/run/denia/service.sock`, starts the hardened workload command as its child, and forwards each accepted Unix-socket stream to `127.0.0.1:<internal_port>` inside the workload network namespace.
- Runtime socket directory preparation rejects a symlinked `<rootfs>/run` path before creating `<rootfs>/run/denia`.
- Service names used in runtime paths are restricted to ASCII alphanumeric, `-`, and `_`.
- The rootfs path must exist, be a real directory, and not be a symlink before launch planning succeeds.
- `argv[0]` and `workdir` must be absolute paths inside the rootfs.
- Environment keys must be non-empty and must not contain `=` or NUL.
- Workload process launch clears Denia's host environment before applying the explicit `process.json` environment.
- Cgroups are created under `<cgroup_root>/<service>/<deployment_id>`.
- CPU and memory limits must be non-zero before cgroup files are written.
- CPU limits are written to cgroup v2 `cpu.max` with a `100000` microsecond period.
- Memory limits are written to cgroup v2 `memory.max`.
- Runtime `setpriv` injection rejects symlinked `/.denia` directories and symlinked `/.denia/setpriv` targets before writing into the rootfs.
- Runtime process launch starts Denia in `__denia_cgroup_launcher` mode first. The launcher writes its own host PID to `cgroup.procs`, writes a ready marker for `Runtime::start`, then `exec`s `unshare`.
- Namespace launch then uses `unshare --user --map-users=0,<base>,<size> --map-groups=0,<base>,<size> --fork --pid --net --mount --propagation private --uts --ipc --mount-proc --root <rootfs> --wd <workdir> -- /.denia/socket-proxy --listen /run/denia/service.sock --connect 127.0.0.1:<internal_port> -- /.denia/setpriv --no-new-privs --bounding-set -all -- <argv...>`.
- If launch fails after preparation, Denia removes the prepared deployment directory and cgroup directory.
- If cgroup placement fails before `unshare` is executed, Denia observes that the cgroup launcher exited without the ready marker and removes the prepared deployment and cgroup directories.
- If a service is started again while Denia still tracks an older child for the same service, Denia stops the older child after the replacement child is tracked.
- When Denia stops a tracked service child, it writes `1` to `cgroup.kill` when the cgroup v2 kill file is available, falls back to killing the tracked launcher process, then removes the tracked deployment runtime directory and cgroup directory.
- When Denia replaces a tracked service child, it uses the same cgroup-wide termination path before removing the replaced child deployment runtime directory and cgroup directory.
- Before start and stop decisions, Denia reaps tracked children that already exited and removes their deployment runtime and cgroup directories.

The normal test suite verifies planning, path safety, and cgroup file preparation using temporary directories. The real `start` path is covered by ignored privileged tests that require `DENIA_RUN_PRIVILEGED_TESTS=1`, root, cgroup v2, and Linux namespace permissions.

## Consequences

### Positive

- Denia now has an executable Linux-native runtime path instead of only a placeholder.
- Runtime file paths are deterministic and reject traversal through unsafe service names.
- Bundle and process-manifest validation catches common unsafe inputs before cgroup or process work starts.
- Rootfs symlink rejection prevents a malicious or malformed bundle from making Denia mutate host paths during launch preparation.
- `setpriv` injection refuses symlink targets inside the untrusted rootfs instead of copying through them as host root.
- Host process environment variables are not inherited by workloads; only the image-derived manifest environment is passed through.
- Workloads receive their own network namespace and private mount propagation instead of sharing host networking or mount propagation state.
- Runtime stdout/stderr now flows into the same per-service logs returned by the management API.
- Acquired external images can now be materialized into the bundle layout consumed by `LinuxRuntime`.
- Git/BuildKit output can now be materialized into the bundle layout consumed by `LinuxRuntime`.
- Rootfs materialization and runtime launch now share explicit user-namespace configuration instead of hard-coded hidden values.
- OCI image process config can now feed the runtime manifest without hand-written process specs.
- External-image and Git service deployment can now exercise acquisition-to-runtime wiring without callers manually constructing a `RootfsBundle` artifact.
- External-image and Git API deployment requests now reach the same acquisition-to-runtime path.
- The management API can now stop the tracked runtime workload for a service and reflect the stopped state in SQLite.
- Recent per-service runtime logs can now be fetched through the management API.
- Runtime CPU and memory usage can now be read from cgroup v2 for the promoted deployment through the management API.
- The TCP-to-Unix bridge data path is implemented, supervised by Denia during promotion and stop, and covered by ignored live network tests for environments that allow loopback TCP and AF_UNIX sockets.
- TCP-only workloads can continue binding their configured internal loopback port while Denia exposes the service through the owned Unix socket and Traefik bridge path.
- The stage-1 socket proxy configures loopback without requiring an `ip` binary inside the rootfs.
- Socket directory preparation avoids following a rootfs-controlled `run` symlink while Denia is preparing paths as host root.
- The cgroup contract is testable without requiring root in the normal test suite.
- Failed launches do not leave prepared per-deployment runtime or cgroup directories behind in the normal failure path.
- Failed cgroup placement no longer allows `unshare --fork` or the workload to start outside Denia's target cgroup.
- Replacement deployment starts no longer leak the previous tracked workload child for the same service.
- Runtime stop and replacement no longer leave stale per-deployment runtime or cgroup directories for tracked children.
- Runtime stop and replacement terminate the workload cgroup as a unit when `cgroup.kill` is available, avoiding orphaned descendants from `unshare --fork`.
- Exited children are reaped during later lifecycle operations so dead tracked processes do not linger indefinitely.
- The privileged launch path remains explicit and opt-in.

### Negative

- `unshare` is currently used as the namespace launcher binary, so hosts must provide util-linux.
- Hosts must provide `skopeo` and `umoci` for external image acquisition and unpacking.
- Runtime launch now depends on the Denia binary being callable in host-side `__denia_cgroup_launcher` mode before entering `unshare`.
- The injected `/.denia/socket-proxy` must be usable inside the target rootfs. A fully static Denia release binary is the expected production packaging shape for scratch/distroless images.
- Broader network device setup, veth links, and DNS policy inside the workload network namespace are not yet complete.
- The live bridge tests are ignored in this sandbox because loopback TCP and AF_UNIX socket binding return `EPERM`; they are intended for environments with local socket permissions.

## Alternatives Considered

- **Use Docker/containerd/runc**: Rejected by ADR-001 and the product requirement to avoid Docker-compatible runtimes for service execution.
- **Direct `clone3` stage-1 supervisor now**: Deferred because it needs a separate privileged implementation and audit surface. The current `unshare` runner makes the contract visible first.
- **Run OCI images directly**: Deferred because Denia needs a rootfs bundle and process manifest boundary before parsing full OCI image config safely.
- **Use `skopeo dir:` as rootfs**: Rejected because `dir:` materializes image transport data, not the unpacked filesystem tree that `LinuxRuntime` needs.

## References

- [ADR-001: Initial Backend Architecture](001-initial-backend-architecture.md)
- `src/runtime.rs`
- `tests/deploy_orchestration.rs`
- `tests/linux_runtime_privileged.rs`
