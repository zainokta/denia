# Spec: In-Process OCI Image Acquisition (Replace skopeo + umoci)

**Status:** Draft — not yet executed
**Date:** 2026-05-25
**Plan:** `docs/superpowers/plans/2026-05-25-inprocess-oci-acquisition.md`
**Related ADR:** `docs/adr/003-linux-runtime-process-runner.md` (to be amended)

---

## Problem

Denia's artifact acquisition path shells out to two external Go binaries via `CommandRunner`:

- `skopeo` — `copy docker://<image> oci:<dir>` (registry pull) and `inspect --config oci:<dir>` (image config read).
- `umoci` — `unpack --image oci:<dir> <bundle>` (apply layers into a rootfs tree).

Both are host prerequisites recorded in ADR-003 (lines 24, 66) as a Negative consequence. They couple Denia to externally installed tooling, complicate host provisioning, and put image-pull/unpack behavior outside Denia's own code and test surface. The control plane is otherwise a self-contained Rust binary.

## Goal

Acquire and unpack external OCI images entirely in-process in Rust, removing the `skopeo` and `umoci` host dependencies. Keep the rootfs-bundle contract consumed by `LinuxRuntime` unchanged.

## Non-Goals

- Replacing `buildctl` (BuildKit), `sops`, `unshare`, `setpriv`, `chown`, or `traefik` — these stay host dependencies until the runtime-hardening / rustix plans say otherwise.
- Changing the `RootfsBundle` / `process.json` contract or the `LinuxRuntime` launch path (ADR-003).
- Introducing Docker, containerd, or runc as the service runtime (forbidden by ADR-001).
- Building/pushing images; this is pull + unpack only. BuildKit still produces OCI layout output for the Git path.
- A hosted registry.

## Background / Current Behavior

`src/artifacts/acquirer.rs`:

- `acquire_external_image` (187–202): `skopeo copy` → returns a digest.
- `inspect_image_config` (226–236): `skopeo inspect --config` → `OciImageConfig` (`Entrypoint`/`Cmd`/`Env`/`WorkingDir`).
- `materialize_rootfs_bundle` (204–224): `umoci unpack` → rootfs tree under `artifact_dir/<safe-digest>/`.
- `acquire_git` (149–185): `buildctl build ... --output type=oci,dest=<artifact_dir>` writes a local OCI layout, then the same unpack path runs.

The deployment coordinator (`src/deploy.rs:127-178`) calls `acquire_rootfs_bundle_from_image_config` for both external-image and Git deploys. Binary paths are configured in `src/config.rs` (`registry_pull_binary`, `oci_unpack_binary`) via `DENIA_REGISTRY_PULL_BINARY` / `DENIA_OCI_UNPACK_BINARY`.

## Requirements

### Functional

1. **R1 — Registry pull (replaces `skopeo copy`).** Given an image reference (e.g. `ghcr.io/acme/web:latest`), pull the manifest, config, and ordered layer blobs over the OCI distribution / Docker Registry v2 protocol. Select the manifest matching `linux/<host arch>` by default; fail closed if absent unless an explicit platform override is configured. Do not silently run `linux/amd64` on non-amd64 hosts.
2. **R2 — Image config (replaces `skopeo inspect --config`).** Parse the pulled config blob into the existing `OciImageConfig` shape; `RootfsBundleSpec::try_from` continues to derive `argv`/`env`/`workdir`.
3. **R3 — Rootfs unpack (replaces `umoci unpack`).** Apply ordered layer tarballs onto a rootfs directory, supporting uncompressed, gzip, and zstd layers, OCI whiteouts (`.wh.<name>`) and opaque-dir markers (`.wh..wh..opq`).
4. **R4 — BuildKit layout path.** Read the on-disk OCI layout written by `buildctl` (`index.json` → manifest → config + layer blobs) and feed the same unpacker. `buildctl` itself is still invoked via `CommandRunner`.
5. **R5 — Registry auth.** Support, keyed by registry host:
   - Anonymous (public ghcr / Docker Hub).
   - Basic (private / self-hosted), credentials sourced from a SOPS secret via the existing `SopsSecretStore`.
   - AWS ECR token exchange (behind cargo feature `ecr`).
   - GCP Artifact Registry / GCR access-token auth (behind cargo feature `gar`).
6. **R6 — Config cleanup.** Remove the `registry_pull_binary` / `oci_unpack_binary` config fields and their env vars; add registry-credential configuration (SOPS secret ref + provider selection).

### Non-Functional / Constraints

- **NF1 — Security (trust boundary).** Tar extraction must not write outside the rootfs: reject `..` traversal and absolute-path escapes; do not follow symlinks out of the rootfs when resolving entry destinations. This is the new security-critical surface and must be test-driven first.
- **NF2 — Integrity.** Verify each layer's sha256 against its descriptor digest; fail closed on mismatch.
- **NF3 — Lean core build.** ECR/GAR dependencies are optional cargo features; default build pulls neither AWS nor Google SDKs.
- **NF4 — Test isolation.** Unit/integration tests must run unprivileged and offline. Real registry pulls are opt-in (`DENIA_RUN_NETWORK_TESTS=1`, `#[ignore]`), mirroring the privileged-test gate in `tests/linux_runtime_privileged.rs`.
- **NF5 — Typed errors at boundaries; no panics for expected failures** (per AGENTS.md Rust conventions).
- **NF6 — No secret logging.** Never log decrypted credentials, tokens, or SOPS payloads.

## Proposed Design (summary)

New `src/oci/` module with two traits:

- `OciImagePuller` — `pull(image) -> PulledImage` (registry, R1/R2/R5) and `read_layout(dir) -> PulledImage` (BuildKit, R4). Concrete `RegistryImagePuller` over `oci-client`.
- `OciRootfsUnpacker` — `unpack(layers, rootfs_dir)` (R3/NF1/NF2). Concrete `TarRootfsUnpacker` over `tar` + `flate2`/`zstd` + `sha2`.

`PulledImage { digest, config: OciImageConfig, layers: Vec<LayerBlob> }` unifies the registry and layout paths so both converge on one unpacker. `ArtifactAcquirer` is rewired to hold `Arc<dyn OciImagePuller>` + `Arc<dyn OciRootfsUnpacker>` and drops all skopeo/umoci `CommandRunner` calls (keeping `buildctl` and any existing post-unpack rootfs ownership step until the rustix syscall refactor replaces `chown`). Auth via `RegistryCredentialProvider` (Anonymous/Basic core; ECR/GAR feature-gated).

Tech: `oci-client`, `oci-spec`, `tar`, `flate2`, `zstd`, `sha2`; `aws-config`/`aws-sdk-ecr` (feature `ecr`); `gcp_auth` (feature `gar`).

See the plan for task-by-task implementation.

## Acceptance Criteria

- [ ] `grep -rn "skopeo\|umoci" src/` returns nothing; `DENIA_REGISTRY_PULL_BINARY` / `DENIA_OCI_UNPACK_BINARY` removed.
- [ ] External-image deploy pulls + unpacks in-process; `process.json` derived from the pulled config; resulting rootfs bundle feeds the unchanged `LinuxRuntime`.
- [ ] Git deploy still builds via `buildctl`, then unpacks the OCI layout in-process.
- [ ] Runtime-security ownership semantics are preserved: if `userns_base` chowning exists before this work, the in-process unpack path still applies it after rootfs creation.
- [ ] Unpacker tests cover: layer application, file overwrite, `.wh.` whiteout, `.wh..wh..opq` opaque dir, zstd, and three security cases (`..` traversal, absolute path, symlink escape) — all green, unprivileged, offline.
- [ ] Layer sha256 mismatch fails the pull.
- [ ] `cargo build` (default), `cargo build --features ecr,gar`, `cargo clippy --all-targets --all-features`, and `cargo test` all pass.
- [ ] Opt-in `DENIA_RUN_NETWORK_TESTS=1 cargo test --test oci_registry_live -- --ignored` pulls a public image and populates a rootfs.
- [ ] Privileged runtime tests still pass with the in-process-produced bundle.
- [ ] ADR-003 amended; README host prerequisites + env table updated.

## Risks

- Safe tar extraction (NF1) is subtle — the reason `umoci` exists; must be covered by tests before the implementation is trusted.
- `oci-client` / `oci-spec` API may have drifted from the versions noted in the plan; confirm at execution (context7).
- ECR/GAR token caching/refresh must avoid per-pull auth round-trips.
- Threading `PulledImage` through `ArtifactAcquirer` is the main structural change; the registry and BuildKit-layout paths must converge cleanly on the unpacker.

## Open Questions

- Registry-credential config shape: single SOPS secret holding a `[{registry, username, password}]` list vs. per-registry secret refs. (Plan assumes the list form.)
- Platform override config shape if Denia ever needs to pull a non-host architecture image; default behavior should fail when the host platform is absent.
