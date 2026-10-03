# ADR-029: Self-Update From Signed GitHub Release Binaries

- Status: Accepted
- Date: 2026-06-01

## Context

Denia installs by building from source (`install.sh` → `make install` →
`cargo build --release --locked` + the pnpm SPA build). Moving a running
install to a newer version meant re-running the whole build by hand. Operators
need a first-class way to upgrade: `denia update`.

The Denia binary is self-contained — the web console SPA is embedded at compile
time via `rust-embed` (ADR-004) — so a single binary per CPU architecture is a
complete, runnable release artifact. Nothing else needs to ship alongside it at
runtime.

Denia already holds a strong supply-chain posture: `install.sh` pins SHA256 for
`rustup-init` and the NodeSource setup script, secrets are SOPS-encrypted, and
the runtime drops capabilities. An update path that fetches and executes a new
binary is a high-value target and must verify what it downloads before trusting
it. The host root filesystem is the explicit trust boundary.

## Decision

Add a privileged `denia update` subcommand that upgrades the host install from a
prebuilt binary published on the project's GitHub Releases, and a GitHub Actions
workflow that produces those releases.

**Distribution.** Releases are cut by pushing a `vX.Y.Z` git tag. CI builds one
release binary per supported architecture (`x86_64`, `aarch64`, glibc >= 2.39),
named `denia-<arch>-linux-gnu`, and publishes them as release assets together
with a `SHA256SUMS` manifest and a detached `SHA256SUMS.minisig` signature. The
manifest includes a signed `# denia-release: vX.Y.Z` header so a manifest from
one release cannot be replayed into another release. The release tag (minus the
leading `v`) must equal the `Cargo.toml` package version; CI fails the build
otherwise.

**Trust chain (fail-closed).** A single minisign (Ed25519) signing key lives only
in CI as an encrypted GitHub Actions secret. Its matching public key is compiled
into every Denia binary (`MINISIGN_PUBKEY` in `src/cli/update.rs`). On update,
the client:

1. verifies `SHA256SUMS.minisig` over the bytes of `SHA256SUMS` with the pinned
   public key (`minisign-verify`, a pure-Rust verify-only crate);
2. verifies the signed `# denia-release: ...` header matches the GitHub
   release's `tag_name`;
3. looks up the downloaded binary's expected SHA256 in the now-trusted
   `SHA256SUMS`;
4. computes the SHA256 of the downloaded binary (`sha2`) and compares.

Any failure at any step aborts before the running binary is touched. TLS to
GitHub protects transport but integrity does not depend on it: an attacker who
controls the release, a mirror, or the transport still cannot forge a
`SHA256SUMS` the pinned key accepts. `--tag` selects a specific release (pin or
downgrade) but is subject to the same signature check, so even downgrades are
authenticated.

**Apply + rollback.** The command runs as root (`privilege::require_root`),
mirroring `rotate-token`. It downloads into `/usr/local/bin` so the final
`rename` over `/usr/local/bin/denia` is atomic on one filesystem; the running
daemon (a separate process) keeps its old inode until restart. Before the swap
the current binary is copied to `/usr/local/bin/denia.bak`. After the swap the
command runs `systemctl restart denia.service` and polls `is-active`. If the
restart or the readiness wait fails, it restores `denia.bak` and restarts again,
then returns an error. The daemon resolves its `socket-proxy` /
`workload-launcher` multi-call paths from `std::env::current_exe()`, so the
restart alone picks up the new binary for those paths — no extra copies to
refresh.

**Version semantics.** The client compares the release `tag_name` against
`env!("CARGO_PKG_VERSION")` with `semver`. Without `--force`, a release that is
not strictly newer is a no-op. `--check` reports status and never downloads.

## Amendment (2026-10-02): Target-release dependency reconciliation

After verifying and installing the new binary, the updater invokes a hidden,
root-only reconciliation command on the absolute installed binary. That binary
supplies its own Zot version, hashes, configuration, and unit. The old updater's
compiled-in pins cannot decide dependencies for a newer release.

Reconciliation must finish and Zot must become active before Denia restarts.
If reconciliation fails, restore the previous Denia binary atomically and do
not restart Denia into the failed release. If Denia itself fails to restart,
restore its binary and attempt to restart the previous release. Report recovery
failures explicitly. Successfully reconciled Zot stays installed if a later
Denia restart fails. `--force` uses the same lifecycle; `--check` returns before
any dependency provisioning, file writes, or service operations.

Targets without the hidden handoff command fail closed, including explicitly
selected older releases. Rollback restores the prior binary rather than guessing
which dependencies that release requires.

An already-published updater that only swaps the binary and restarts has no
dependency handoff. Installing a release containing this amendment cannot
retroactively change that running process. A deployment upgrading from such a
release requires a transition release carrying the handoff before it relies on
Zot. The existing non-root systemd unit
cannot safely provision a root-owned dependency during daemon startup. This PR
does not add daemon privileges to hide that rollout requirement.

The rollout is two releases. Backport the dependency-neutral updater handoff
to a transition release that keeps the existing registry backend and unit. Its
internal reconciliation command has no managed registry dependency to install.
Once hosts have installed that updater through their normal signed update,
publish the Zot-enabled release. The transition updater invokes reconciliation
from the newly installed Zot-enabled binary before restarting Denia. Do not
offer a direct jump from a pre-handoff release as an automatically provisioned
upgrade. Creating and publishing that transition release is release work beyond
this PR's existing branch update.

## Consequences

- Easier: `sudo denia update` upgrades an install in seconds without a host
  toolchain; `--check` makes "is there an update?" a one-liner.
- Easier: releases are reproducible and signed; the verification logic ships in
  the same binary it protects.
- Harder: the project now must keep the minisign key secret and never lose it;
  losing it forces a key rotation (re-pin the new public key, ship a transition
  release verifiable by the old key). The signing key is the new crown jewel.
- Harder: two new dependencies (`semver`, `minisign-verify`) and a release CI
  pipeline to maintain. Both are small and contained.
- Constraint: only glibc >= 2.39 `x86_64`/`aarch64` are published, matching the
  baseline enforced by `install.sh`, `denia doctor`, and `denia update`. Older
  glibc hosts are unsupported rather than relying on source builds that cannot
  later self-update.

## Alternatives Considered

- **Rebuild from source on update** (`git fetch` the tag + `make install`).
  Rejected: requires the full Rust + Node + pnpm toolchain on every production
  host, is slow, and has many more failure modes than verifying one binary.
- **Checksum only, no signature.** Rejected: trusts GitHub (and anyone who can
  publish to the release) for the checksum manifest's integrity. The signature
  moves trust to a key Denia controls, matching the existing SHA-pinning posture.
- **cosign / sigstore keyless.** Rejected for now: heavier (OIDC, Rekor, the
  cosign verifier) than a self-update needs. minisign is a single key, a tiny
  pure-Rust verifier, and a pinned public key — the minimal trustworthy design.
- **`self_update` crate.** Rejected: pulls a large dependency tree and its
  signature story does not match a pinned-minisign-over-SHA256SUMS model; the
  hand-rolled path is small and auditable.

## References

- ADR-004 (embed web console — why one binary is a complete artifact)
- ADR-025 (CLI-driven host provisioning — the privileged-subcommand pattern)
- minisign: <https://jedisct1.github.io/minisign/>
- `minisign-verify`: <https://docs.rs/minisign-verify/>
