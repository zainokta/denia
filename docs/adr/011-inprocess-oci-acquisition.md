# ADR-011: In-Process OCI Image Acquisition

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

ADR-003 mandated `skopeo` and `umoci` as host binaries for external-image
acquisition and rootfs unpack. Both are written in Go, add ~200 MB of host
disk + a static dependency surface for operators, and shell out from the
control plane on every deploy. Folding both into the Denia binary cuts
operator setup and the failure modes of `Command::new`.

## Decision

- Introduce two traits in `src/oci/`:
  - `OciImagePuller::pull(image) -> PulledImage` and
    `OciImagePuller::read_layout(dir) -> PulledImage`.
  - `OciRootfsUnpacker::unpack(layers, rootfs_dir)`.
- Provide `RegistryImagePuller` (`oci-client`), `TarRootfsUnpacker`
  (`tar` + `flate2` + `zstd`), and `StaticCredentialProvider` (Anonymous +
  Basic). ECR/GAR providers are deferred behind future cargo features.
- Rewire `ArtifactAcquirer` to hold `Arc<dyn OciImagePuller>` and
  `Arc<dyn OciRootfsUnpacker>`. External-image acquisition pulls in-memory
  and unpacks; Git/Dockerfile builds use BuildKit's OCI layout output, then
  read it via `read_layout` and unpack via the same `TarRootfsUnpacker`.
- Drop `registry_pull_binary` and `oci_unpack_binary` from `AppConfig`. The
  related `DENIA_REGISTRY_PULL_BINARY` and `DENIA_OCI_UNPACK_BINARY` env
  vars are gone.
- Bundle ownership adjustment (`syscall::chown::recursive_lchown`) tolerates
  `Operation not permitted` (test environments without `CAP_CHOWN`); under
  privileged production runs it still enforces uid/gid mapping.

## Consequences

- Host setup drops two binaries; Denia is the sole orchestrator on the deploy
  path.
- Registry credentials for private images now live in
  `StaticCredentialProvider` (basic auth) — no `~/.docker/config.json`
  parsing yet. ECR/GAR are deferred; operators wanting them must keep
  external tooling until those providers land.
- `OciImagePuller::pull` returns the entire image in memory before unpack.
  For very large images (multi-GB) this trades disk staging (skopeo's
  approach) for RAM. Acceptable for the single-node operator console;
  streaming pull is a future optimization.

## Alternatives Considered

- **Continue with `skopeo`/`umoci`**: rejected; two extra host-installs and
  two extra `Command::new` failure paths.
- **`containerd`/`runc`**: rejected per ADR-003 — Denia is not adopting a
  container runtime daemon.
- **Streaming layer apply**: deferred; in-memory is simpler and correct
  enough for the single-node target.

## References

- `docs/superpowers/plans/2026-05-25-inprocess-oci-acquisition.md`
- `docs/superpowers/specs/2026-05-25-inprocess-oci-acquisition.md`
- Amends ADR-003 (Linux Runtime Process Runner).
