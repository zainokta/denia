# Denia-Managed Traefik (Supervised Host Process) — Design

Date: 2026-05-27
Status: Draft (brainstormed, pending implementation plan)
Relates to: TODO #11 (install.sh — slims down once Traefik is self-managed).
New ADR-016 required (runtime + ingress change).

## Problem

Today Traefik is a **separate, operator-installed daemon**. Denia only generates
Traefik's dynamic file-provider config (`DENIA_TRAEFIK_DYNAMIC_CONFIG`, default
`/etc/traefik/dynamic/denia.yml`) and owns the loopback bridge listeners Traefik
forwards to. Bringing up a node therefore requires a manual Traefik install +
static-config wiring + ACME setup before ingress works.

We want Denia to **own Traefik end to end**, the way Dokploy ships and runs its
own Traefik: Denia acquires Traefik, writes its static + dynamic config, runs it,
restarts it on crash, and shuts it down with the control plane. The operator
should not install or configure Traefik separately.

## Goals

- Denia is the **sole owner** of Traefik lifecycle, config, and certs in the
  default configuration. No external/system Traefik required.
- Acquire Traefik as an **OCI image** pulled in-process (reuse existing
  `oci-client` path), pinned by digest.
- Run Traefik as a **supervised host child process** (no namespaces) so it binds
  host `:80`/`:443` and reads/writes its config + cert files directly.
- Generate Traefik **static config** (entrypoints, file provider, ACME resolver)
  and keep emitting **dynamic config** as today.
- ACME via **HTTP-01** using `certResolver` `le`, email required.
- **Default on**; an explicit escape hatch disables management for sites that run
  their own edge.

## Non-Goals

- Running Traefik inside Denia's namespace sandbox (rejected: edge needs host net
  + host ports + persistent cert/config files; see "Rejected: workload-Traefik").
- Introducing Docker/containerd/runc (Denia stays Docker-free).
- DNS-01 / TLS-ALPN challenges, multiple resolvers, custom middlewares beyond the
  existing HTTP→HTTPS redirect (deferred).
- cgroup limits on the Traefik child (possible later; out of scope v1).
- Multi-node / HA Traefik.

## Decisions

1. **Supervised host process, not a sandboxed workload.** An edge proxy needs the
   opposite of the workload sandbox: host network (bind `:80`/`:443`), host file
   access (static config + `acme.json` persistence), and to sit in *front* of the
   loopback bridges rather than behind a socket-proxy. Running it as a plain
   supervised child avoids punching holes (host-net mode, bind mounts, cap
   retention, boot reconciler) in the deliberate isolation posture of ADR-005.

2. **OCI image as the binary source.** Reuse the in-process `oci-client` puller +
   `OciRootfsUnpacker` to fetch `traefik` and run the unpacked binary directly.
   This keeps "no manual install" without a separate download/checksum path and
   without Docker. Pinned by digest for reproducibility.

3. **Managed mode default-on, authoritative.** Denia fully owns Traefik's config
   and lifecycle. `DENIA_MANAGE_TRAEFIK=0` is an explicit escape hatch that
   reverts to today's external-Traefik behavior (Denia only writes dynamic config
   at the legacy path; no supervision).

4. **ACME HTTP-01, email required.** Simplest correct path with no DNS provider
   creds. If managed mode is on, TLS is in use, and `DENIA_ACME_EMAIL` is unset →
   fail fast at config load (`ConfigError`).

5. **Resolver name stays `le`.** The dynamic-config renderer already emits
   `tls.certResolver: le` (`acme_resolver`, default `le`). The generated static
   config defines a matching `le` resolver so the two halves line up.

## Configuration (`src/config.rs`)

New fields on `AppConfig`:

| Field | Env | Default | Notes |
|-------|-----|---------|-------|
| `manage_traefik` | `DENIA_MANAGE_TRAEFIK` | `true` | `0`/`false` → external mode |
| `traefik_image` | `DENIA_TRAEFIK_IMAGE` | `docker.io/library/traefik:v3.<x>@sha256:<pin>` | pinned by digest |
| `acme_email` | `DENIA_ACME_EMAIL` | — | required if managed + TLS |
| `http_port` | `DENIA_HTTP_PORT` | `80` | `web` entrypoint |
| `https_port` | `DENIA_HTTPS_PORT` | `443` | `websecure` entrypoint |

Derived:

- `traefik_dir = data_dir/traefik` — holds `rootfs/`, `traefik.yml`, `acme.json`,
  `dynamic/`, `.image-digest`.
- Dynamic-config path resolution:
  - **Managed mode:** default `traefik_dir/dynamic/denia.yml` (so the generated
    static config's file provider watches `traefik_dir/dynamic`).
  - **External mode:** unchanged default `/etc/traefik/dynamic/denia.yml`.
  - `DENIA_TRAEFIK_DYNAMIC_CONFIG` overrides in both modes.

New `ConfigError` variant: `AcmeEmailRequired` (managed + TLS-in-use + no email).
Note: "TLS in use" is determined at startup from existing services
(`tls_enabled`); if none yet, missing email is allowed until the first TLS
service is created (validated again at that point) — OR, simpler v1: require
`DENIA_ACME_EMAIL` whenever managed mode is on. **Decision: require it whenever
managed mode is on** (predictable, avoids deferred failure). Document in README.

## Acquisition (`src/oci/`)

Add a helper (e.g. `pull_image_to_dir(image, dest_rootfs) -> Result<digest>`):

1. Pull manifest + layers via the existing `RegistryImagePuller` (anonymous auth
   for Docker Hub public image).
2. Unpack layers into `traefik_dir/rootfs` via `OciRootfsUnpacker`.
3. Write the resolved manifest digest to `traefik_dir/.image-digest`.

Digest cache: on boot, if `.image-digest` matches the configured image's resolved
digest, skip pull/unpack. (For a digest-pinned ref the comparison is local; for a
tag, a lightweight manifest HEAD resolves the digest first.)

Binary path: `traefik_dir/rootfs/usr/local/bin/traefik` (official image layout).

## Static Config Generation (`src/ingress/traefik_supervisor.rs`, new)

Render `traefik_dir/traefik.yml`:

```yaml
entryPoints:
  web:
    address: ":80"        # http_port
  websecure:
    address: ":443"       # https_port
providers:
  file:
    directory: "<traefik_dir>/dynamic"
    watch: true
certificatesResolvers:
  le:
    acme:
      email: "<acme_email>"
      storage: "<traefik_dir>/acme.json"
      httpChallenge:
        entryPoint: web
```

Pure function `render_static_config(&TraefikStaticOptions) -> String` for unit
testing. Ensure `traefik_dir/dynamic/` exists and seed an empty `denia.yml` (or
a `{}` file) so Traefik starts cleanly before the first deployment.

`acme.json` must be created mode `0600` if absent (Traefik refuses world-readable
ACME storage).

## Supervisor (`src/ingress/traefik_supervisor.rs`, run from `src/main.rs`)

A `TraefikSupervisor` with an async run loop, spawned at boot independently of
`axum::serve`:

1. Acquire (pull/unpack or cache-hit).
2. Write static config; ensure `dynamic/` + `acme.json`.
3. Spawn child: `traefik --configfile=<traefik_dir>/traefik.yml`, cwd
   `traefik_dir`. No namespaces; inherits root (binds privileged ports).
4. Pipe child stdout/stderr → `log_dir/traefik.log`.
5. Watchdog: on child exit, restart with exponential backoff (1s → cap 30s; reset
   after stable uptime).
6. Shutdown: on control-plane `ctrl_c`, send SIGTERM to the child, await exit with
   a timeout, then SIGKILL fallback.

Failure isolation: if acquisition fails, log + retry with backoff. The control
plane keeps serving on `bind_addr` (admin reaches `IP:7180` directly), so a
Traefik problem never deadlocks management.

Inject a spawn abstraction (trait or fn pointer) so restart/backoff logic is
unit-testable without pulling a real image or binding ports.

`src/main.rs` wiring: when `config.manage_traefik`, spawn the supervisor task
alongside the scheduler; thread the existing shutdown signal to it.

## Dynamic Config (mostly unchanged)

`render_file_provider_config` + `rerender_traefik` (`src/deploy/routes.rs`) are
unchanged except for the path they write to, which already comes from
`config.traefik_dynamic_config_path`. In managed mode that path resolves under
`traefik_dir/dynamic/`. Traefik's file provider (`watch: true`) hot-reloads on
write — same contract as today.

## Privileges / Isolation

The Traefik child runs rootful on the host network by design: it is the
trust-boundary edge. This is consistent with "host root is the trust boundary"
(AGENTS.md). It does **not** weaken workload isolation — user workloads keep full
namespace isolation + cap drop; only the edge proxy (which Denia itself controls,
not user code) runs unsandboxed. Optional cgroup confinement of the child is
deferred.

## Bootstrapping / Ordering

- Fresh boot: control plane binds `bind_addr` → supervisor pulls+starts Traefik →
  Traefik serves `:80`/`:443`, file provider watches `dynamic/`.
- Control-plane domain (`DENIA_CONTROL_DOMAIN`, ADR-007) routes through Traefik to
  `control_backend_addr` (`http://bind_addr`). Until Traefik is up, the operator
  reaches the API directly at `IP:7180`. No hard ordering deadlock.

## Rejected: Traefik as a sandboxed workload

Considered running Traefik through Denia's runtime as a "system workload" (share
host netns, retain `CAP_NET_BIND_SERVICE`, bind-mount config + cert dirs, skip
socket-proxy/bridge, add a boot reconciler). Rejected for v1: it requires several
new runtime capabilities that each punch a hole in ADR-005's isolation posture and
add a system-service class + boot reconciliation. The supervised-host-process
approach delivers the same operator outcome (no manual install, Denia owns
Traefik) at far lower architectural cost.

## Testing

### Backend (Rust)

- `render_static_config`: entrypoint ports reflect `http_port`/`https_port`; file
  provider directory = `traefik_dir/dynamic`, `watch: true`; ACME block carries
  `email`, `storage`, `httpChallenge.entryPoint: web`; resolver named `le`.
- Config: managed mode without `DENIA_ACME_EMAIL` → `ConfigError::AcmeEmailRequired`.
- Config: `DENIA_MANAGE_TRAEFIK=0` → no supervisor; dynamic path defaults to the
  legacy `/etc/traefik/dynamic/denia.yml`.
- Digest cache: matching `.image-digest` → no re-pull; mismatch → re-pull.
- Supervisor restart: injected fake spawner that "exits" triggers backoff +
  restart; backoff schedule monotonic up to the cap and resets after stable
  uptime; shutdown signal stops the loop and terminates the child.
- `acme.json` created `0600` when absent.

### Manual / integration

- On a host with cgroup v2: managed boot pulls Traefik, serves `:80`, issues a
  cert for a verified domain via HTTP-01, hot-reloads on a new deployment.

## Verification Commands

- `cargo build`
- `cargo test`
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`

## Files Touched (anticipated)

- `src/config.rs` — new fields, env parsing, `AcmeEmailRequired`, path resolution.
- `src/ingress/traefik_supervisor.rs` — new: static-config render + supervisor.
- `src/ingress/mod.rs` — export the module.
- `src/oci/` — `pull_image_to_dir` helper (reuse puller + unpacker).
- `src/main.rs` — spawn supervisor under `manage_traefik`; shutdown wiring.
- `docs/adr/016-managed-traefik.md` — new ADR (runtime + ingress).
- `README.md` — env table (new vars), managed-Traefik section.
- `AGENTS.md` — update the "Traefik integration" project convention.

## Follow-ups

- TODO #11 install.sh: drop the Traefik install/config step; it now only handles
  toolchain deps, build, `denia` group + sudoers drop-in, EnvironmentFile token
  (`/etc/denia/denia.env`, `0640 root:denia`), systemd unit, and the bootstrap
  superadmin URL (`?token=` flow, admin-bootstrap design). Separate spec.
- Optional cgroup confinement of the Traefik child.
- Optional DNS-01 / multi-resolver support.
