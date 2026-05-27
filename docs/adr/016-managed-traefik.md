# ADR-016: Denia-Managed Traefik

- **Status**: Accepted
- **Date**: 2026-05-27

## Context

Traefik was an **operator-installed daemon** external to Denia. Denia only wrote
Traefik's dynamic file-provider config (`DENIA_TRAEFIK_DYNAMIC_CONFIG`, default
`/etc/traefik/dynamic/denia.yml`) and owned the loopback bridge listeners
Traefik forwarded to. Bringing up a node therefore required a manual Traefik
install, static-config wiring, and ACME setup before any ingress would work.

This is operator friction. We want Denia to **own Traefik end to end** — the
way Dokploy ships and runs its own Traefik — so the operator never installs or
configures Traefik separately. Denia acquires, configures, runs, supervises, and
shuts down Traefik as part of its own lifecycle.

## Decision

**Denia pulls the official Traefik OCI image in-process** using the existing
`oci-client` puller and `TarRootfsUnpacker` (ADR-011, ADR-015), pinned to
`docker.io/library/traefik:v3.3` (overridable via `DENIA_TRAEFIK_IMAGE`).
Only the statically-linked `traefik` binary is consumed from the unpacked
rootfs; the rest of the Alpine image is not relied upon. The host CA trust store
(`/etc/ssl/certs`) is used for ACME TLS — no chroot, no libc dependency.

**Denia runs Traefik as a supervised host child process** (no namespaces) so it
binds `:80` and `:443` directly on the host network. The supervisor:

- Pulls and caches the binary (digest-keyed under `<data_dir>/traefik/`);
  skips re-pull when `.image-digest` matches.
- Writes a static config (`traefik.yml`) defining:
  - `web` entrypoint on `:80` (`DENIA_HTTP_PORT`, default `80`).
  - `websecure` entrypoint on `:443` (`DENIA_HTTPS_PORT`, default `443`).
  - File provider watching `<data_dir>/traefik/dynamic/`, `watch: true`.
  - ACME `certificatesResolvers.le` via HTTP-01 (`httpChallenge.entryPoint: web`),
    email from `DENIA_ACME_EMAIL`; `acme.json` created mode `0600`.
- Keeps emitting the dynamic config to `<data_dir>/traefik/dynamic/denia.yml`
  (the path previously written to `/etc/traefik/dynamic/denia.yml`; still
  overridable via `DENIA_TRAEFIK_DYNAMIC_CONFIG`).
- Restarts on crash with exponential backoff (1 s → cap 30 s; resets after
  stable uptime).
- Shuts down gracefully (SIGTERM → timeout → SIGKILL) when the control plane
  stops.

**`DENIA_ACME_EMAIL` is required only when a service has `tls_enabled`.**
Nodes with no TLS service boot without it. A missing email when TLS is in use
produces a `ConfigError::AcmeEmailRequired` at startup and at service
create/update.

**Management is unconditional.** There is no `DENIA_MANAGE_TRAEFIK` flag and no
external-Traefik mode. The operator must not run a separate Traefik instance;
Denia is the sole owner of `:80`/`:443` on the node.

## Consequences

**Operator must stop any pre-existing Traefik.** On upgrade, the dynamic-config
path moves from `/etc/traefik/dynamic/denia.yml` to
`<data_dir>/traefik/dynamic/denia.yml`, and Denia starts its own Traefik. If an
external Traefik still holds those ports, the supervised child hits `EADDRINUSE`;
the supervisor treats this as a fatal, non-retried error and logs a clear
message ("`:80`/`:443` already in use — stop the external Traefik, Denia now
manages its own") rather than crash-looping. This is a known v1 limitation:
precise fatal detection relies on the child's own exit log rather than on a
pre-spawn bind probe.

**Config hot-reload; restart drops in-flight connections.** Dynamic config
changes are hot-reloaded by Traefik's file-provider watcher without restarting
the process. A process restart (crash recovery or shutdown) is not graceful for
active connections — no `SO_REUSEPORT` socket handoff. This is an accepted v1
limitation.

**`traefik.log` has no rotation.** The supervisor appends child stdout/stderr to
`<log_dir>/traefik.log` with no rotation in v1; rely on the operator's
logrotate or note it as a follow-up.

**SELinux/AppArmor may block exec.** A rootful host process exec'ing a binary
from `data_dir` can be denied under enforcing SELinux (wrong file context) or a
restrictive AppArmor profile. Document a relabel/exception step; exec-permission
errors are surfaced with guidance. Not solved in-code for v1.

**Traefik is a pure-Go static binary.** The official release is
`CGO_ENABLED=0`; it needs no dynamic loader and works on the host without the
rest of the unpacked Alpine rootfs. A startup smoke-test (`traefik version`)
verifies the binary executes before entering the serve loop; failure produces a
fatal error with guidance rather than a crash loop.

**Failure isolation.** If Traefik acquisition fails the control plane continues
serving on `bind_addr` (`IP:7180`). A Traefik problem never deadlocks
management API access.

## Alternatives Considered

- **Keep operator-installed Traefik (status quo):** requires manual install,
  static-config wiring, and ACME setup on every node. Rejected — the goal is
  zero Traefik setup for the operator.
- **Run Traefik as a sandboxed workload** (share host netns, retain
  `CAP_NET_BIND_SERVICE`, bind-mount config/cert dirs, add a boot reconciler):
  requires punching several holes in ADR-005's isolation posture and adds a
  system-service class. Rejected for v1 — supervised host process delivers the
  same operator outcome at far lower architectural cost.
- **Ship a Traefik binary in the Denia release tarball:** avoids an OCI pull at
  boot but duplicates the distribution and version-pinning problem. Rejected in
  favour of reusing the in-process OCI acquisition path already present.

## References

- `docs/superpowers/specs/2026-05-27-managed-traefik-design.md`
- ADR-005 (Runtime Security Hardening) — isolation posture Traefik deliberately
  does not share.
- ADR-007 (Ingress + TLS) — ingress model and ACME certResolver `le` that the
  dynamic config already emits.
- ADR-011 (In-Process OCI Image Acquisition) — puller + unpacker reused to
  fetch the Traefik binary.
- ADR-015 (Streaming OCI Layer Staging) — streaming layer staging used during
  acquisition.
