# Streaming OCI Layer Staging — Design

- **Date**: 2026-05-27
- **Status**: Approved (brainstorm)
- **Topic**: Very large images load fully into RAM (no streaming yet)

## Problem

OCI image acquisition holds every compressed layer in RAM as `LayerBlob.data: Vec<u8>`.
For multi-GB images this risks OOM on the single-node control plane. ADR-011
flagged this explicitly:

> `OciImagePuller::pull` returns the entire image in memory before unpack. For
> very large images (multi-GB) this trades disk staging (skopeo's approach) for
> RAM. Acceptable for the single-node operator console; streaming pull is a
> future optimization.

Three buffer points hold whole layers:

- `src/oci/registry.rs:65` — `oci-client` `pull()` buffers all layers in memory,
  then `layer.data.to_vec()` copies again (double buffer).
- `src/oci/layout.rs:68` — `std::fs::read(&blob_path)` reads each blob fully into a `Vec`.
- `src/oci/mod.rs:25` — `LayerBlob.data: Vec<u8>` holds the bytes.

The unpack side already streams: `unpack.rs` wraps `&layer.data[..]` in a
`Box<dyn Read>` decoder and extracts entry-by-entry via tar `Archive` + per-entry
`io::copy`, with size/count caps. Only the **compressed layer bytes** are the RAM
problem — not the uncompressed tree.

## Decision

Stage each layer to a file on disk, then unpack from the file. Bounded RAM
(~one chunk buffer); transient disk cost ≈ compressed image size. Verify-before-extract
is preserved on **both** acquisition paths (registry and BuildKit OCI layout).

Rejected alternatives:

- **Fully streaming pipeline** (registry → decoder → tar → rootfs, no staging):
  lowest RAM and disk, but digest verifiable only *after* writing the rootfs,
  requiring rollback of a partially-extracted tree on mismatch. Weakens
  verify-before-use; rejected.
- **Hybrid threshold** (small layers in RAM, large layers staged): best runtime
  profile but two code paths to maintain and test; rejected as premature.

## Design

### 1. Data model (`src/oci/mod.rs`)

`LayerBlob` stops owning bytes and points at a file on disk:

```rust
pub struct LayerBlob {
    pub digest: String,
    pub compression: LayerCompression,
    pub path: PathBuf,            // was: data: Vec<u8>
}

pub struct PulledImage {
    pub digest: String,
    pub config: OciImageConfig,
    pub layers: Vec<LayerBlob>,
    _staging: Option<tempfile::TempDir>, // RAII cleanup of staged registry blobs
}
```

- Drop `#[derive(Clone)]` on both structs. The `Vec<u8>` clone is gone; `TempDir`
  is not `Clone`. No `PulledImage`/`LayerBlob` value is cloned today — only field
  reads (`pulled.digest.clone()`), which are unaffected.
- Registry path: layers staged into a per-pull `TempDir`; the guard lives in
  `PulledImage` and drops after `unpack` returns → automatic cleanup, including
  on error paths.
- Layout path: `path` references the existing `blobs/sha256/<hex>` file —
  zero copy, no staging guard (`_staging: None`).

### 2. Registry pull (`src/oci/registry.rs`)

Replace `client.pull(...)` (buffers all layers) with:

1. `pull_manifest_and_config(image, auth)` → `(OciImageManifest, manifest_digest, config_json)`.
   Multi-arch index resolution is handled internally by `oci-client`.
2. Parse `config_json` → `OciImageConfig`.
3. `TempDir::new_in(staging_dir)`. For each `desc` in `manifest.layers`:
   open a `tokio::fs::File`, call `client.pull_blob(image, &desc, file)` — streams
   the blob to disk and **verifies the layer digest inline** (`oci-client`'s
   `layer_digester` compares against the descriptor digest).
4. `compression` derived from `desc.media_type` (gzip / zstd / none), same matching
   as today.
5. The manual `Sha256` re-hash at `registry.rs:67-75` is **removed** — `pull_blob`
   performs verification during streaming.
6. `PulledImage.digest = manifest_digest`.

`RegistryImagePuller::new(staging_dir: PathBuf)` gains a staging base directory,
set to `config.artifact_dir` (same filesystem as the final rootfs — avoids
cross-device staging). Constructor call updated at `src/artifacts/acquirer.rs:83`.

### 3. Layout read (`src/oci/layout.rs`)

- Drop `std::fs::read(&blob_path)` for layer blobs (`layout.rs:68`).
- **Verify-first**: stream-hash each on-disk blob file (`Sha256`, chunked read),
  compare to the manifest layer digest (`sha256:<hex>`). On mismatch return
  `OciError::DigestMismatch`. This *adds* verification the current layout path
  lacks — chosen for parity with the registry path.
- On success, set `LayerBlob.path = blob_path`. `_staging: None`.
- Manifest and config JSON continue to be read fully into memory (tiny).

### 4. Unpack (`src/oci/unpack.rs`)

Single change to the reader source: replace `&layer.data[..]` with
`BufReader::new(std::fs::File::open(&layer.path)?)`, fed into the same
Gzip / Zstd / None decoder. Streaming extraction (tar `Archive`, per-entry
`io::copy`, `MAX_UNCOMPRESSED_BYTES` / `MAX_SINGLE_FILE_BYTES` / `MAX_FILE_COUNT`
caps, whiteout handling, `safe_join`) is unchanged.

### 5. Dependencies (`Cargo.toml`)

- `tempfile`: promote from `[dev-dependencies]` to `[dependencies]`.
- `tokio`: add the `"fs"` feature (for `pull_blob`'s `AsyncWrite` file target).

### 6. Error handling

- `pull_blob` digest mismatch (`oci-client` `DigestError`) → mapped to
  `OciError::Pull` (or `DigestMismatch`).
- Layout hash mismatch → `OciError::DigestMismatch { expected, actual }`.
- File I/O during staging / unpack → `OciError::Io`.
- Partial-failure cleanup is automatic via `TempDir` drop.

### 7. Tests

- `unpack.rs` `gz_layer` helper: write the gz bytes to a temp file and point
  `path` at it instead of inline `data`. Existing extract / overwrite / whiteout /
  opaque-dir / traversal tests then exercise the file-backed path.
- New layout test: build an OCI layout dir (`index.json`, manifest, config,
  layer blobs), call `read_layout`, assert `LayerBlob.path` references the on-disk
  blob and a digest-mismatched blob is rejected.
- Registry streaming: extend the existing mock harness if one covers the puller;
  otherwise gate behind an integration test (real/mocked registry).

### 8. ADR

New **ADR-015: Streaming OCI Layer Staging**, amending ADR-011 consequence #3.
Records the reversal: bounded RAM + transient on-disk staging (≈ compressed image
size under `artifact_dir`) replaces the in-RAM-whole-image decision. Required per
AGENTS.md (dependency change + acquisition-behavior change).

## Profile

- **RAM**: O(oci-client chunk buffer) + per-layer `BufReader`. No whole-layer buffering.
- **Disk**: transient, ≈ compressed image size under `artifact_dir`; reclaimed when
  `TempDir` drops after unpack (registry) / already-resident BuildKit output (layout).
- **Security**: verify-before-extract on both paths. Registry digest verified by
  `oci-client` during streaming; layout digest verified by stream-hash before unpack.

## Verification

- `cargo build`
- `cargo test`
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`
