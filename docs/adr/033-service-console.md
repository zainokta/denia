# ADR-033: Service Console

- Status: Accepted
- Date: 2026-06-03
- Related: ADR-003, ADR-005, ADR-019, ADR-024, ADR-025

## Context

Operators need an interactive way to inspect a deployed service from inside Denia's Linux runtime isolation. The console must behave like an exec session into a running service replica, not a Docker shell, SSH session, or diagnostic clone. Denia owns service runtime isolation and must preserve per-service namespace, cgroup, filesystem, and auth boundaries.

## Decision

Denia will add a live service console exposed through the management API, web console, and the `denia console` subcommand of the existing single binary.

- The console attaches to a selected live replica of the service's promoted deployment.
- The runtime launches `/bin/sh` through a new PTY-backed `setns` path that joins the target replica's namespaces and cgroup.
- The existing `spawn_namespaced_process` service/job launcher remains unchanged.
- The browser and CLI first create a short-lived single-use console ticket through bearer-authenticated HTTP, then open a websocket using that ticket.
- Websocket binary frames carry terminal input/output bytes. Text JSON frames carry readiness, resize, exit, close, and error control messages.
- Console sessions are limited process-wide and per service.
- Denia records metadata-only audit events: user/principal, service id, deployment id, replica index, session id, start/end time, and exit reason. The exit reason is the real outcome of the console child — `exit code N`, `signal N`, or `unknown` — captured by reaping the child (see below), not a placeholder.
- The console shell is reaped on session end. The bridge sends `SIGTERM`, waits a bounded grace period, escalates to `SIGKILL` for a wedged or `SIGTERM`-ignoring shell, then `waitpid`s it so it cannot linger as a zombie holding the replica's namespaces open. The runtime additionally keeps a backstop set of live console pids that its child reaper sweeps, so a dropped bridge task still gets the child collected. The captured exit status feeds both the audit "exit reason" and the protocol `exit` frame's `code`.
- The console child re-applies the workload's per-launch privilege floor before `execve`: `no_new_privs`, capability-bounding-set drop, and the same seccomp denylist the service launcher installs (ADR-005). It already inherits the replica's user namespace (capless versus the host); this closes the asymmetry where the interactive shell ran without the workload's syscall filter and privilege floor.
- The console child re-validates the target replica's identity (the `/proc/<pid>/stat` start-time captured when the request resolved the live replica) after joining the namespace fds and aborts if the pid was recycled, closing the PID-reuse TOCTOU window on `setns`.
- Denia does not persist terminal input or output.
- `/bin/sh` is the only v1 shell. Images without `/bin/sh` return a clear console error.
- The `denia console` command lives inside the unified `denia` binary alongside the existing operator subcommands (ADR-025). A dedicated client-only binary build is out of scope for this ADR; it remains future work behind its own ADR.

## Consequences

- Operators can inspect environment, files, process state, and runtime behavior from the service's actual sandbox.
- Browser websocket auth does not expose bearer tokens in URLs.
- CLI users get a `kubectl exec`-style workflow from the same binary that provisions the host.
- Runtime code gains a new privileged syscall surface for PTY and namespace joining, requiring normal unit tests plus gated privileged tests.
- Distroless images without `/bin/sh` cannot use v1 console until Denia adds an explicit command mode.

## Alternatives Considered

- Diagnostic clone console: rejected because it is not the live service instance.
- Token in websocket query string for the bearer token: rejected because bearer tokens can leak through URLs; a single-use ticket is used instead.
- Full transcript persistence: rejected because console output can include secrets.
- Docker/containerd/runc exec: rejected by Denia's runtime architecture.

## References

- ADR-003: Linux Runtime Process Runner
- ADR-005: Runtime Security Hardening
- ADR-019: Per-Replica Runtime Filesystem Isolation
- ADR-024: Async Deployments With Per-Deployment Log Stream
- ADR-025: CLI-Driven Host Provisioning
