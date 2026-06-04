# ADR-037: Cross-Platform Client via cfg-Gated Single Crate + crates.io Publish

- Status: Proposed
- Date: 2026-06-04
- Supersedes: ADR-030 (cross-platform client command split)
- Related: ADR-004, ADR-025, ADR-029, ADR-033, ADR-034

## Context

`denia` is one crate that builds one multi-personality binary: the daemon (no
subcommand), host provisioning (`setup`/`uninstall`/`status`/`doctor`/
`rotate-token`/`update`), and the developer client commands (`auth`/`push`/
`console`, ADR-034). The daemon, runtime, ingress, and persistence link
Linux-only code — `rustix` with the `mount`/`thread` features, pingora/
boringssl, bundled rusqlite — and `src/lib.rs` declared every module
unconditionally, so the binary only compiled on Linux. `release.yml` therefore
shipped Linux-only assets, and a macOS/Windows developer had no `denia auth` /
`denia push`.

ADR-034 deferred the fix to "its own ADR" (the ADR-030 command split). Two
constraints shape that fix now:

1. The client commands are portable — a coupling audit shows
   `src/cli/client/{auth,push,console,http,manifest,pack,profile}.rs` import no
   server modules in production code and use only portable crates; `console`
   uses no Linux syscalls. Only `pack.rs`'s `#[cfg(test)]` round-trip touches
   the server's `api::uploads` extractor.
2. The project wants **exactly one crate published to crates.io, named
   `denia`** (`cargo install denia`). crates.io forbids publishing a crate with
   path dependencies on unpublished crates, so a multi-crate workspace would
   force publishing the helper crates too.

A first draft of this decision proposed a Cargo workspace
(`denia-common` + `denia-client` + `denia`). Constraint 2 rules it out: it would
publish three crates and give the client a separate binary name. This ADR adopts
the single-crate alternative instead.

## Decision

Keep **one** `denia` crate (no workspace) and make the Linux-only surface
conditional, so the same crate builds a full server on Linux and a client-only
binary on macOS/Windows.

- **Module gating.** `src/lib.rs` keeps `cli` unconditional and gates every
  server module (`api`, `app`, `daemon`, `runtime`, `ingress`, `state`,
  `secrets`, `oci`, `registry`, `scheduler`, `web`, `syscall`, …) behind
  `#[cfg(target_os = "linux")]`. `src/cli/mod.rs` gates the host subcommands and
  their dispatch arms; `auth`/`push`/`console` stay unconditional. `main.rs`
  gates the `socket-proxy`/`workload-launcher` multi-call entry and the daemon
  arm; on non-Linux, no subcommand is an error rather than a daemon start.
- **Dependency gating.** Linux-only crates move under
  `[target.'cfg(target_os = "linux")'.dependencies]` (pingora, rustix, rusqlite,
  boring, instant-acme, oci-client, rust-embed, axum, tower(-http), tracing, …).
  Portable crates the client needs stay in `[dependencies]` (clap, reqwest,
  tokio, crossterm, tokio-tungstenite, futures-util, tar, zstd, ignore, serde,
  serde_json, toml, uuid, hex, thiserror, tempfile, anyhow).
- **One binary, two reach.** The single `denia` bin is the full server on Linux
  and `auth`/`push`/`console` on macOS/Windows — same command name everywhere,
  so `denia auth` / `denia push` work on every platform.
- **crates.io.** Publish the one `denia` crate. The embedded web console
  (`rust-embed`, Linux-only) is a build-output directory excluded by
  `.gitignore`, so the crate force-includes the prebuilt SPA via
  `[package].include` and CI runs `pnpm build` before `cargo publish`.
  `cargo install denia` then yields the full server on Linux and the client on
  macOS/Windows.
- **Release workflow.** `release.yml` keeps the signed Linux server assets
  (ADR-029) and adds: a `client` job building the crate for
  `x86_64`/`aarch64-apple-darwin` and `x86_64-pc-windows-msvc` (client-only
  binaries, no web build, no mold) folded into `SHA256SUMS` + signature; and a
  `publish-crates` job that runs `cargo publish` on a tag, gated on a
  `CARGO_REGISTRY_TOKEN` secret (skips when unset).

## Consequences

- Easier: one crates.io crate; `cargo install denia` works on Linux (server) and
  macOS/Windows (client) with the same `denia auth` / `denia push` UX.
- Easier: no separate client crate or binary name to keep in version lockstep;
  the shared types stay in their existing modules.
- Harder: `#[cfg(target_os = "linux")]` is sprinkled across `lib.rs`,
  `cli/mod.rs`, and `main.rs`. A future edit can re-introduce a Linux dep into a
  portable module and only break the macOS/Windows build, which is **verified in
  CI** (mac/Windows runners) — locally only the *dependency* gating is checkable
  (`cargo tree --target …` shows pingora/rustix/rusqlite absent).
- Harder: the published crate carries a frozen SPA snapshot and an `include`
  allowlist that must stay in sync, and `cargo install denia` builds from
  unsigned source — a different trust path than ADR-029's minisign-verified
  binaries (which remain the recommended Linux install). crates.io's ~10 MiB
  compressed size cap must fit `src` + the SPA bundle.

## Alternatives Considered

- **Cargo workspace split** (`denia-common` + `denia-client` + `denia`).
  Rejected here: publishing exactly one crate named `denia` is impossible while
  it path-depends on unpublished helper crates, so the split would publish three
  crates and give the client a separate binary name. The workspace gives a
  stronger structural boundary, but the one-crate requirement outweighs it.
- **`publish = false` server; publish only client crates.** Rejected: the goal
  is exactly one crate named `denia`.
- **Keep Linux-only; no cross-platform client.** Rejected: leaves macOS/Windows
  developers with no `denia auth` / `denia push`, the original gap.

## References

- ADR-030 (cross-platform client CLI — superseded; this records its resolution)
- ADR-034 (client-driven deploy via working-tree upload — deferred this split)
- ADR-029 (self-update from signed GitHub release — Linux server, separate
  trust path from `cargo install`)
- ADR-033 (service console — client scaffolding: profiles, `ClientApi`)
- ADR-004 (embedded web console — Linux-gated; bundled into the published crate)
- ADR-025 (CLI-driven host provisioning — Linux-only subcommands)
- `.github/workflows/release.yml`
