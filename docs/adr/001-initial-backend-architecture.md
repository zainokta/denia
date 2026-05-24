# ADR-001: Initial Backend Architecture

## Status

Accepted

## Date

2026-05-24

## Context

Denia is a backend-first PaaS inspired by Dokploy, Coolify, CapRover, and Komodo. The first capability is deploying services from Dockerfile-based projects or external images, hosting them on the machine through Traefik, and collecting CPU, memory, and related usage metrics.

Denia intentionally avoids Docker as the service runtime. Dockerfile compatibility is still required, but the runtime boundary must be Denia-owned and based on Linux primitives.

## Decision

Denia v1 is a single-node Rust backend using `axum`. The same binary contains the HTTP control plane and node agent, separated internally so they can split later if multi-node support is accepted.

The supported host baseline is Ubuntu/Debian LTS with systemd and cgroup v2. The Denia agent runs rootful because it owns namespaces, mounts, cgroups, sockets, process lifecycle, cleanup, and metric collection. Host root remains the trust boundary.

The control plane uses SQLite for durable local state: services, credentials metadata, artifacts, deployments, runtime status, allocated bridge ports, routes, and recent metric snapshots.

Secrets are stored as SOPS-encrypted files, not raw SQLite values. V1 uses a host-local age identity with root-only permissions. SQLite stores secret references only. SSH deploy keys and registry token/basic credentials use this model.

Denia supports two artifact sources in v1:

- Git over SSH, built from an explicit Dockerfile path and context path through BuildKit.
- External OCI image pulls from public registries or registries using SOPS-backed token/basic credentials.

BuildKit is used only to convert Dockerfile sources into OCI artifacts/rootfs bundles. Denia does not use Docker, containerd, or runc to execute services in v1.

The custom runtime owns process isolation and lifecycle. It applies default CPU/memory limits with per-service overrides, injects environment, tracks cgroup/procfs metrics, captures logs, exposes a Denia-owned Unix socket per workload, and cleans up failed starts.

Traefik is the main reverse proxy through its file provider. Denia generates Traefik config. Because Traefik's normal service targets are host/port oriented, Denia creates loopback-only bridge listeners that forward Traefik traffic to the Denia-owned service sockets.

Deployments are health-gated. Denia starts the new deployment, waits for the explicit HTTP health check path and timeout, then atomically promotes routing and retains the old deployment for rollback.

## Consequences

### Positive

- Keeps the first backend focused on a single machine while preserving clear control-plane boundaries.
- Provides Dockerfile compatibility without making Docker the runtime.
- Keeps secrets encrypted at rest and avoids raw secret storage in SQLite.
- Makes Traefik integration explicit and independent of Docker labels/providers.
- Keeps service metrics available through Denia APIs before any frontend exists.

### Negative

- A custom runtime is significantly harder than wrapping Docker, containerd, or runc.
- Rootful operation means host compromise or agent compromise is high impact.
- The Traefik loopback bridge is an extra component Denia must supervise.
- Multi-node scheduling, hosted registry push APIs, and rootless operation are deferred.

## Alternatives Considered

- **Docker runtime**: Rejected because Denia's core requirement is to avoid Docker for running workloads.
- **containerd/runc runtime**: Rejected for v1 because it weakens the custom-runtime requirement, but it remains a possible future ADR if reliability outweighs runtime ownership.
- **Custom Dockerfile builder**: Rejected because implementing Dockerfile semantics is a separate large system. BuildKit gives compatibility while Denia focuses on runtime ownership.
- **Hosted OCI registry in v1**: Rejected because push-compatible registries require Distribution API support, blob lifecycle, auth scopes, retention, and garbage collection.
- **PostgreSQL**: Rejected for v1 because SQLite is enough for single-node state and avoids external operations.
- **Rootless runtime first**: Rejected because user namespaces, networking, mounts, and cgroup behavior would delay the core deploy path.

## References

- BuildKit: <https://docs.docker.com/build/buildkit/>
- SOPS: <https://getsops.io/>
- Traefik file provider: <https://doc.traefik.io/traefik/providers/file/>
- Docker post-install root-equivalent socket context: <https://docs.docker.com/engine/install/linux-postinstall/>
