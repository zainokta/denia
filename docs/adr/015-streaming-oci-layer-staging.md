# ADR-015: Streaming OCI Layer Staging

- **Status**: Proposed
- **Date**: 2026-05-27

## Context

ADR-011 folded OCI image acquisition into the Denia binary and accepted a known
limitation in its consequences:

> `OciImagePuller::pull` returns the entire image in memory before unpack. For
> very large images (multi-GB) this trades disk staging (skopeo's approach) for
> RAM. Acceptable for the single-node operator console; streaming pull is a
> future optimization.

`LayerBlob` owned `data: Vec<u8>`, so every compressed layer of every pulled
image was held in RAM at once — both on the registry path (`oci-client`'s
`Client::pull` buffers all layers, then we copied them again) and the BuildKit
layout path (`std::fs::read` of each on-disk blob). A multi-GB image could OOM
the single-node control plane. This ADR delivers the deferred streaming
optimization.

## Decision

- `LayerBlob` no longer owns bytes; it carries `path: PathBuf` pointing at a file
  on disk. The decompress + tar-extract pipeline (already streaming, entry by
  entry) reads each layer from its file.
- Registry pull streams: `Client::pull_manifest_and_config` (resolves multi-arch
  indexes internally and returns the small config JSON), then `Client::pull_blob`
  streams each layer descriptor to a temp file. `pull_blob` verifies the layer
  digest inline as it streams, so verification still happens before extraction.
- BuildKit OCI layout read references the already-on-disk blob directly (zero
  copy) after a streaming SHA-256 check against the manifest's layer digest —
  this *adds* layer-digest verification the layout path previously lacked, for
  parity with the registry path.
- Registry layers are staged into a per-pull `tempfile::TempDir` created under
  `artifact_dir` (same filesystem as the final rootfs). The guard lives in
  `PulledImage` and reclaims the staged blobs by RAII after unpack — including on
  error paths. Layout blobs are not staged (they already live in `artifact_dir`).
- `tempfile` is promoted from a dev-dependency to a runtime dependency; the
  `tokio` `fs` feature is enabled for the async file write target.

## Consequences

- Bounded RAM: only `oci-client`'s chunk buffer plus a per-layer `BufReader`,
  instead of whole-image buffering. Multi-GB images no longer risk OOM.
- Transient disk cost ≈ the compressed image size under `artifact_dir` while a
  registry pull is in flight, reclaimed when `PulledImage` drops. This partially
  reverses ADR-011's "disk for RAM" trade — we now use bounded disk staging
  again, but without reintroducing `skopeo`.
- Verify-before-extract is preserved on both acquisition paths.
- `PulledImage` and `LayerBlob` lose their `Clone` derive (owning a `TempDir` is
  not `Clone`, and nothing cloned these values).

## Alternatives Considered

- **Fully streaming pipeline** (registry → decoder → tar → rootfs, no staging):
  lowest RAM and disk, but the layer digest could only be verified *after* the
  rootfs was written, requiring rollback of a partially-extracted tree on
  mismatch. Rejected — weakens verify-before-use.
- **Hybrid threshold** (small layers in RAM, large layers staged to disk): best
  runtime profile but two code paths to maintain and test. Rejected as premature.
- **Keep buffering in RAM** (ADR-011 status quo): rejected — the OOM risk is the
  problem this ADR exists to remove.

## References

- `docs/superpowers/specs/2026-05-27-streaming-oci-layer-staging-design.md`
- `docs/superpowers/plans/2026-05-27-streaming-oci-layer-staging.md`
- Amends ADR-011 (In-Process OCI Image Acquisition) — delivers the streaming
  pull deferred in its consequences.
