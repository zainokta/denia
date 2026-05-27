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
- ACME via **HTTP-01** using `certResolver` `le`, email required when TLS is used.
- **Always managed — no opt-out.** Denia is the only Traefik on the node; there is
  no external-Traefik mode and no `DENIA_MANAGE_TRAEFIK` flag.

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

3. **Always managed, authoritative — no opt-out.** Denia fully owns Traefik's
   config and lifecycle on every node. There is no flag to disable it and no
   external-Traefik mode: the operator must not run a separate Traefik. This is
   the Dokploy model (the platform ships and runs its own edge). An operator who
   already runs Traefik must stop it (the node's `:80`/`:443` belong to Denia's
   Traefik).

4. **ACME HTTP-01, email required when TLS is used.** Simplest correct path with
   no DNS provider creds. If any service has `tls_enabled` and `DENIA_ACME_EMAIL`
   is unset → fail fast (`ConfigError` at startup; same check at service
   create/update). Nodes with no TLS service need no email.

5. **Resolver name stays `le`.** The dynamic-config renderer already emits
   `tls.certResolver: le` (`acme_resolver`, default `le`). The generated static
   config defines a matching `le` resolver so the two halves line up.

6. **We consume only the Traefik binary, and require it to be statically
   linked.** The official Traefik release binary is pure-Go, `CGO_ENABLED=0`
   (statically linked); it needs no dynamic loader and reads the *host* CA trust
   store (`/etc/ssl/certs`) via Go's `crypto/x509`. Denia therefore execs
   `rootfs/usr/local/bin/traefik` directly on the host without chroot and does
   **not** depend on the rest of the unpacked Alpine rootfs (so the unpacker's
   rejection of absolute symlink targets — `src/oci/unpack.rs` — is irrelevant
   here; only the single binary is used). A startup smoke-test (`traefik
   version`) verifies the binary actually executes on this host before entering
   the serve loop; if it fails (e.g. a future non-static image), the supervisor
   surfaces a fatal error with guidance instead of crash-looping. A
   chroot-into-rootfs fallback is explicitly out of scope for v1.

7. **ACME (`/.well-known/acme-challenge`) and Denia domain verification
   (`/.well-known/denia-challenge`) are distinct and coexist.** They occupy
   different path prefixes and serve different purposes; see "ACME vs Denia
   domain verification" below. Neither replaces the other.

## Configuration (`src/config.rs`)

New fields on `AppConfig`:

| Field | Env | Default | Notes |
|-------|-----|---------|-------|
| `traefik_image` | `DENIA_TRAEFIK_IMAGE` | `docker.io/library/traefik:v3.<x>@sha256:<pin>` | pinned by digest |
| `acme_email` | `DENIA_ACME_EMAIL` | — | required when any service uses TLS |
| `http_port` | `DENIA_HTTP_PORT` | `80` | `web` entrypoint |
| `https_port` | `DENIA_HTTPS_PORT` | `443` | `websecure` entrypoint |

There is **no** `DENIA_MANAGE_TRAEFIK` flag — supervision is unconditional.

Derived:

- `traefik_dir = data_dir/traefik` — holds `rootfs/`, `traefik.yml`, `acme.json`,
  `dynamic/`, `.image-digest`.
- Dynamic-config path: always `traefik_dir/dynamic/denia.yml`, so the generated
  static config's file provider watches `traefik_dir/dynamic`.
  `DENIA_TRAEFIK_DYNAMIC_CONFIG` still overrides the path for advanced setups,
  but Denia always supervises its own Traefik regardless.

### Upgrade path

An existing node upgrading to this version switches behavior on next boot: the
dynamic-config path moves from the old `/etc/traefik/dynamic/denia.yml` default to
`traefik_dir/dynamic/denia.yml`, and Denia starts its own Traefik on
`:80`/`:443`. **The operator must stop any pre-existing Traefik** — there is no
opt-out. If a separate Traefik still holds those ports, the supervised child hits
`EADDRINUSE`; the supervisor treats this as a **fatal, non-retried** error with an
explicit message ("`:80`/`:443` already in use; stop the external Traefik —
Denia now manages its own") rather than infinite backoff. Called out in README
upgrade notes.

New `ConfigError` variant: `AcmeEmailRequired` (TLS-in-use + no email). "TLS in
use" is determined at startup from existing services (`tls_enabled`); the same
check runs at service create/update so enabling TLS without `DENIA_ACME_EMAIL`
fails at the API boundary. Nodes with no TLS service boot without an email.
Document in README.

## Acquisition (`src/oci/`)

Add a helper (e.g. `pull_image_to_dir(image, dest_rootfs) -> Result<digest>`):

1. Pull manifest + layers via the existing `RegistryImagePuller` (anonymous auth
   for Docker Hub public image).
2. Unpack layers into a **temp dir** under `traefik_dir`, then atomically rename
   to `traefik_dir/rootfs` (avoids a half-unpacked rootfs being treated as
   valid if the process crashes mid-extraction).
3. Verify the binary exists at `rootfs/usr/local/bin/traefik` and passes the
   `traefik version` smoke-test (Decision 6).
4. Only after (2)+(3) succeed, write the resolved manifest digest to
   `traefik_dir/.image-digest`.

Digest cache: on boot, if `.image-digest` matches the configured image's resolved
digest **and** the binary is present, skip pull/unpack. For a digest-pinned ref
the comparison is local. For a tag, resolving the remote digest may require a full
manifest GET if `oci-client`/`RegistryImagePuller` cannot do a HEAD-only resolve —
the helper falls back to GET; this is acceptable (boot-time, infrequent).

The supervisor is the **sole writer** of `traefik_dir` (single task), so no
concurrent-pull locking is needed.

Binary path: `traefik_dir/rootfs/usr/local/bin/traefik` (official image layout).
Only this binary is consumed; the rest of the rootfs is not relied upon
(Decision 6). CA trust comes from the host (`/etc/ssl/certs`).

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
   after stable uptime). **Exception:** if the child fails to bind a listen port
   (`EADDRINUSE`), treat it as fatal and stop retrying (see Upgrade path) — log a
   clear message rather than crash-looping.
6. Shutdown: on control-plane `ctrl_c`, send SIGTERM to the child, await exit with
   a timeout, then SIGKILL fallback.

Failure isolation: if acquisition fails, log + retry with backoff. The control
plane keeps serving on `bind_addr` (admin reaches `IP:7180` directly), so a
Traefik problem never deadlocks management.

**Restart drops in-flight connections.** A binary restart is not graceful for
active connections (no socket handoff / `SO_REUSEPORT`). Config changes are hot
(file-provider `watch: true`) and do not restart the process; only crashes and
shutdown do. This is an accepted v1 limitation.

Inject a spawn abstraction (trait or fn pointer) so restart/backoff logic is
unit-testable without pulling a real image or binding ports.

`src/main.rs` wiring: always spawn the supervisor task alongside the scheduler;
thread the existing shutdown signal to it.

## Dynamic Config (mostly unchanged)

`render_file_provider_config` + `rerender_traefik` (`src/deploy/routes.rs`) are
unchanged except for the path they write to, which already comes from
`config.traefik_dynamic_config_path`. In managed mode that path resolves under
`traefik_dir/dynamic/`. Traefik's file provider (`watch: true`) hot-reloads on
write — same contract as today.

## ACME vs Denia domain verification (coexistence)

Two distinct `/.well-known/` flows run on the `web` (`:80`) entrypoint; they do
not collide because they own different path prefixes:

- **`/.well-known/acme-challenge/…`** — served **internally by Traefik's own
  HTTP-01 solver** (configured via `httpChallenge.entryPoint: web`). No dynamic
  router is generated for this; Traefik intercepts the prefix itself when issuing
  a cert for the `le` resolver. This is new with managed mode.
- **`/.well-known/denia-challenge/…`** — Denia's existing per-service **domain
  ownership verification** (ADR-013). `render_file_provider_config`
  (`src/ingress/traefik.rs`) emits a high-priority router on `web` that forwards
  this prefix to `control_backend_addr`. This is **retained unchanged**: it
  verifies that a domain points at this node *before* the domain is added to the
  routable set — a separate concern from issuing the TLS cert.

Sequencing for a TLS-enabled custom domain: (1) operator adds the domain → Denia
verifies ownership via `denia-challenge`; (2) once verified, the domain enters
the dynamic config with `tls.certResolver: le`; (3) Traefik then obtains the cert
via `acme-challenge`. The spec keeps both routers; the implementation must NOT
remove or remap the `denia-challenge` router, and must NOT add a `denia` router
for `acme-challenge` (Traefik owns it).

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
- Config: a TLS service without `DENIA_ACME_EMAIL` → `ConfigError::AcmeEmailRequired`
  (at startup and at service create/update). No TLS service → boots without email.
- Dynamic path always resolves to `traefik_dir/dynamic/denia.yml` (no opt-out).
- Digest cache: matching `.image-digest` → no re-pull; mismatch → re-pull.
- Supervisor restart: injected fake spawner that "exits" triggers backoff +
  restart; backoff schedule monotonic up to the cap and resets after stable
  uptime; shutdown signal stops the loop and terminates the child.
- `EADDRINUSE` from the fake spawner → fatal, no retry (not backoff).
- Acquisition atomicity: a fake unpacker that fails mid-way leaves no
  `.image-digest` and no `rootfs` (temp-dir rename not performed); next boot
  re-pulls. Binary-presence/smoke-test failure → fatal with guidance.
- `acme.json` created `0600` when absent.

### Manual / integration

- On a host with cgroup v2: managed boot pulls Traefik, serves `:80`, issues a
  cert for a verified domain via HTTP-01, hot-reloads on a new deployment.

## Operational Notes / Known Limitations

- **SELinux/AppArmor:** a rootful host process exec'ing a binary from `data_dir`
  may be denied under enforcing SELinux (wrong file context) or an AppArmor
  profile. Document a relabel/exception step; surface exec-permission errors with
  guidance. Not solved in-code for v1.
- **`traefik.log` growth:** the supervisor appends child stdout/stderr to
  `log_dir/traefik.log` with no rotation in v1. Note size-cap/rotation as a
  follow-up (or rely on the operator's logrotate).
- **Restart drops in-flight connections** (see Supervisor) — accepted v1
  limitation.

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
