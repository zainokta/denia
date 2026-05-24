# Denia

Denia is a Docker-free, single-node PaaS. It deploys and runs services with a
Denia-owned Linux runtime (namespaces + cgroup v2) instead of Docker,
containerd, or runc, and exposes a versioned `/v1` management API behind a
bearer admin token.

It is built for solo operators and homelab users running self-hosted workloads
on a single node: deploy services, manage routes and secrets, and read real
cgroup/procfs runtime metrics. The goal is a tool you trust enough to forget
about, opening it only to do a thing and leave.

> Status: **v1, single-node.** Multi-node scheduling, hosted registry push, and
> rootless operation are intentionally deferred. See the ADRs.

## Architecture

A single Rust binary contains both the HTTP control plane and the node agent,
separated internally so they can split later if a multi-node ADR is accepted.

- **HTTP API** — `axum`, versioned under `/v1`, protected by a bearer admin token.
- **State** — SQLite (`rusqlite`, bundled) for services, credentials metadata,
  artifacts, deployments, runtime status, bridge ports, routes, and recent
  metric snapshots.
- **Secrets** — SOPS-encrypted files; SQLite stores **references only**, never
  raw secret values. Default backend is a host-local age identity with
  root-only permissions.
- **Artifacts** — two v1 sources: Git over SSH built via BuildKit, and external
  OCI image pulls (`skopeo` copy + `umoci unpack` into a rootfs bundle).
  BuildKit is used only to turn Dockerfiles into OCI artifacts — never as the
  runtime.
- **Runtime** — `LinuxRuntime` launches workloads with
  `unshare --fork --pid --mount --uts --ipc --mount-proc --root <rootfs> --wd <workdir>`,
  places them in `<cgroup_root>/<service>/<deployment_id>` cgroups, applies
  CPU/memory limits, and cleans up failed starts. Host root is the trust
  boundary; the agent runs rootful.
- **Ingress** — Traefik via its file provider. Denia generates route config and
  owns loopback bridge listeners that forward Traefik traffic to Denia-owned
  Unix sockets per workload.
- **Metrics** — read from cgroup v2 and procfs.

Source modules (`src/`): `app` (router + handlers), `domain` (service/credential/
deployment/runtime types), `state` (SQLite store), `secrets` (SOPS), `artifacts`
(acquisition), `runtime` (Linux runner), `deploy` (health-gated coordinator),
`bridge`, `traefik`, `logs`, `metrics`, `command` (process runner abstraction),
`config`, `web` (embedded dashboard).

Deployments are **health-gated**: Denia starts the new deployment, waits for the
configured HTTP health-check path and timeout, then atomically promotes routing
and retains the previous deployment for rollback.

## Requirements

- Rust 2024 edition (stable toolchain).
- Linux host with **cgroup v2** and **systemd** (Ubuntu/Debian LTS baseline).
- `unshare` (util-linux), `skopeo`, `umoci`, `sops`, and (for Git sources)
  BuildKit (`buildctl`) available on `PATH` or configured via env.
- For building the dashboard: `pnpm` + Node (TanStack Start). See `web/`.

## Build & Run

```bash
cargo build                 # baseline build
cargo build --release       # embeds the web dashboard from web/dist/client
```

The release binary embeds the built SPA (`web/dist/client`) via `rust-embed`. In
debug builds the assets are read from disk. Build the frontend first when you
want the console served:

```bash
cd web && pnpm install && pnpm build
```

Run the control plane:

```bash
export DENIA_ADMIN_TOKEN=<your-token>   # required
cargo run --release
```

The server binds `127.0.0.1:7180` by default and serves the API under `/v1`,
with the dashboard as a fallback for non-API routes.

## Configuration

All configuration is environment-driven (`src/config.rs`).

| Variable | Default | Purpose |
|----------|---------|---------|
| `DENIA_ADMIN_TOKEN` | — (**required**) | Bearer token for `/v1` |
| `DENIA_BIND_ADDR` | `127.0.0.1:7180` | Listen address |
| `DENIA_DATA_DIR` | `/var/lib/denia` | Root for state, artifacts, runtime, logs |
| `DENIA_DATABASE_PATH` | `<data_dir>/denia.sqlite3` | SQLite path |
| `DENIA_BUILDKIT_BINARY` | `buildctl` | BuildKit client binary |
| `DENIA_SOPS_BINARY` | `sops` | SOPS binary |
| `DENIA_REGISTRY_PULL_BINARY` | `skopeo` | OCI image copy binary |
| `DENIA_OCI_UNPACK_BINARY` | `umoci` | Rootfs bundle unpack binary |
| `DENIA_TRAEFIK_DYNAMIC_CONFIG` | `/etc/traefik/dynamic/denia.yml` | Generated Traefik file-provider config |

Derived paths: `runtime/`, `artifacts/`, and `logs/` under `DENIA_DATA_DIR`.

## API

`GET /healthz` is public. Everything under `/v1` requires
`Authorization: Bearer <DENIA_ADMIN_TOKEN>`.

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/healthz` | Liveness probe (public) |
| `POST` | `/v1/credentials/git` | Register a Git deploy-key credential (SOPS ref) |
| `POST` | `/v1/credentials/registry` | Register a registry credential (SOPS ref) |
| `GET` | `/v1/services` | List services |
| `POST` | `/v1/services` | Create/update a service config |
| `POST` | `/v1/deployments` | Create a deployment (Git or external image) |
| `GET` | `/v1/services/{id}/deployments` | List deployments for a service |
| `GET` | `/v1/services/{id}/logs` | Service logs |
| `GET` | `/v1/services/{id}/metrics` | Runtime metric snapshots |
| `POST` | `/v1/services/{id}/{action}` | Lifecycle command (start/stop/etc.) |

Credentials store only a `secret_ref` pointing at a SOPS-encrypted file; raw
secret material never enters SQLite or logs.

## Dashboard

The operator dashboard lives in `web/` (TanStack Start / React 19, with an
Effect logic layer beneath TanStack Query). It is mono-forward and dark-primary
— see `PRODUCT.md` and `DESIGN.md` for the product brief and design system. The
built client is embedded into the release binary and served as the fallback for
non-`/v1` routes. Frontend work is out of scope for backend changes unless
explicitly requested.

## Verification

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets --all-features

# privileged runtime tests (root, namespaces, mounts, cgroup v2) — opt-in:
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

Privileged runtime tests are gated because they require root and mutate
namespaces, mounts, and cgroups. Normal CI stays unprivileged and covers
planning, path safety, and cgroup-file preparation against temp directories.

## Security

- Host root is the explicit trust boundary; the agent runs rootful by design.
- Never commit secrets, local keys, or generated private config.
- Never log passwords, tokens, SSH private keys, registry credentials, or
  decrypted SOPS payloads.
- External input is validated at API and runtime boundaries (service names,
  secret refs, process manifests).

## References

- `docs/adr/README.md` and the ADRs (`001` backend architecture, `002` frontend
  Effect layer, `003` Linux runtime process runner).
- `CLAUDE.md` — agent/contributor guidelines.
- [Rust](https://www.rust-lang.org/) · [Axum](https://docs.rs/axum/) ·
  [SOPS](https://getsops.io/) ·
  [Traefik file provider](https://doc.traefik.io/traefik/providers/file/)
