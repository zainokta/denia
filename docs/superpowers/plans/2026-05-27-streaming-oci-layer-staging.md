# Streaming OCI Layer Staging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop holding whole compressed OCI layers in RAM; stage each layer to a file on disk and unpack from it, with verify-before-extract on both registry and BuildKit-layout paths.

**Architecture:** `LayerBlob` stops owning `Vec<u8>` and points at a file path. The registry puller streams each layer blob to a temp file via `oci-client`'s `pull_blob` (which verifies the digest inline); the BuildKit layout path references the already-on-disk blob after a streaming hash check. A per-pull `TempDir` held in `PulledImage` reclaims staged registry blobs by RAII after unpack. Unpack reads each layer from its file through the existing Gzip/Zstd/tar streaming pipeline (unchanged).

**Tech Stack:** Rust 2024, `oci-client` 0.17, `tokio` (`fs`), `tempfile`, `sha2`, `tar`, `flate2`, `zstd`.

Spec: `docs/superpowers/specs/2026-05-27-streaming-oci-layer-staging-design.md`

---

## File Structure

- `Cargo.toml` — promote `tempfile` to a normal dep; add `tokio` `"fs"` feature.
- `src/oci/mod.rs` — `LayerBlob` becomes path-based; `PulledImage` gains a staging guard; drop `Clone` derives.
- `src/oci/unpack.rs` — reader source becomes a file opened from `layer.path`; test helper writes layers to temp files.
- `src/oci/layout.rs` — verify-first stream-hash, then reference the on-disk blob path.
- `src/oci/registry.rs` — stream layers to temp files via `pull_manifest_and_config` + `pull_blob`; constructor gains a staging dir.
- `src/artifacts/acquirer.rs` — pass `config.artifact_dir` into `RegistryImagePuller::new`.
- `docs/adr/015-streaming-oci-layer-staging.md` — new ADR amending ADR-011.

> **Note on atomicity:** Task 2 is a single cross-cutting type change. `LayerBlob`'s
> field change breaks every producer/consumer at once, so the model, both producers
> (registry, layout), the consumer (unpack), and call sites must change together to
> compile. Its sub-steps are ordered so the build/tests are green only at the end of
> the task; do not commit mid-task.

---

### Task 1: Bump dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Move `tempfile` to runtime deps and add the tokio `fs` feature**

In `[dependencies]`, change the `tokio` line to include `"fs"`:

```toml
tokio = { version = "1", features = ["fs", "io-util", "macros", "net", "process", "rt-multi-thread", "signal", "time"] }
```

Add to `[dependencies]` (keep alphabetical neighbours intact):

```toml
tempfile = "3"
```

Remove the `tempfile = "3"` line from `[dev-dependencies]` (it is now inherited from `[dependencies]`).

- [ ] **Step 2: Verify it builds**

Run: `cargo build`
Expected: PASS (no behavior change yet).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build(oci): promote tempfile to runtime dep, enable tokio fs"
```

---

### Task 2: Convert layer transport to file-backed + stream the registry pull

**Files:**
- Modify: `src/oci/mod.rs:14-33`
- Modify: `src/oci/unpack.rs:42-50` (reader) and `:230-247` (test helper) + test call sites
- Modify: `src/oci/layout.rs:50-84`
- Modify: `src/oci/registry.rs` (whole `pull` + struct/constructor)
- Modify: `src/artifacts/acquirer.rs:80-86`

- [ ] **Step 1: Find every `LayerBlob` / `PulledImage` constructor**

Run: `cargo build 2>/dev/null; grep -rn "LayerBlob {\|PulledImage {\|RegistryImagePuller::new\|RegistryImagePuller::default\|\.data\b" src/`
Expected: a list including `mod.rs`, `registry.rs`, `layout.rs`, `unpack.rs` (tests), `acquirer.rs`. Note any test mocks (e.g. an in-memory puller) that build these — they must be updated in this task too.

- [ ] **Step 2: Change the data model in `src/oci/mod.rs`**

Replace the `LayerBlob` and `PulledImage` definitions (lines 21-33) with:

```rust
#[derive(Debug)]
pub struct LayerBlob {
    pub digest: String,
    pub compression: LayerCompression,
    pub path: std::path::PathBuf,
}

#[derive(Debug)]
pub struct PulledImage {
    pub digest: String,
    pub config: OciImageConfig,
    pub layers: Vec<LayerBlob>,
    pub(crate) _staging: Option<tempfile::TempDir>,
}
```

(`LayerCompression` keeps its existing `#[derive(Debug, Clone)]`.)

- [ ] **Step 3: Update the unpack reader in `src/oci/unpack.rs`**

Add `use std::io::BufReader;` is not needed (fully-qualify). Replace the reader block (lines 42-50) with:

```rust
    let file = std::fs::File::open(&layer.path)?;
    let buf = std::io::BufReader::new(file);
    let reader: Box<dyn Read> = match layer.compression {
        LayerCompression::Gzip => Box::new(GzDecoder::new(buf)),
        LayerCompression::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(buf)
                .map_err(|e| OciError::Io(std::io::Error::other(e)))?;
            Box::new(decoder)
        }
        LayerCompression::None => Box::new(buf),
    };
```

- [ ] **Step 4: Update the unpack test helper to write layers to files**

Replace `gz_layer` (lines 230-247) with a version that takes a directory and a name:

```rust
    fn gz_layer(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> LayerBlob {
        let mut tar = Builder::new(Vec::new());
        for (path, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, *data).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, enc.finish().unwrap()).unwrap();
        LayerBlob {
            digest: "sha256:test".into(),
            compression: LayerCompression::Gzip,
            path,
        }
    }
```

Update each test that calls `gz_layer` to pass `dir.path()` (the existing `tempdir`) and a unique file name. Examples:

```rust
    // single_gzip_layer_extracts
    let layer = gz_layer(dir.path(), "l0.tar.gz", &[("hello.txt", b"world")]);
    // second_layer_overwrites
    let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("a.txt", b"v1")]);
    let l2 = gz_layer(dir.path(), "l2.tar.gz", &[("a.txt", b"v2")]);
    // whiteout_deletes_file
    let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("foo", b"x")]);
    let l2 = gz_layer(dir.path(), "l2.tar.gz", &[(".wh.foo", b"")]);
    // opaque_dir_clears_prior
    let l1 = gz_layer(dir.path(), "l1.tar.gz", &[("d/old.txt", b"x")]);
    let l2 = gz_layer(dir.path(), "l2.tar.gz", &[("d/.wh..wh..opq", b""), ("d/new.txt", b"y")]);
```

- [ ] **Step 5: Add a `DigestMismatch` stream-hash to `src/oci/layout.rs`**

Add at the top: `use std::io::Read;` and `use sha2::{Digest, Sha256};`. In the layer loop (lines 50-77), replace the `std::fs::read` + push with:

```rust
            let blob_path = layout_dir.join("blobs").join("sha256").join(layer_hex);
            verify_blob_digest(&blob_path, layer_digest)?;

            layers.push(LayerBlob {
                digest: layer_digest.to_string(),
                compression,
                path: blob_path,
            });
```

Set `_staging: None` in the returned `PulledImage`. Add this helper at the bottom of the file:

```rust
fn verify_blob_digest(path: &Path, expected: &str) -> Result<(), OciError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| OciError::Layout(format!("cannot read layer blob: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(OciError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("sha256:{}", hex::encode(hasher.finalize()));
    if actual != expected {
        return Err(OciError::DigestMismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}
```

- [ ] **Step 6: Stream the registry pull in `src/oci/registry.rs`**

Add `use std::path::PathBuf;`. Replace the struct + `new` + `Default` with:

```rust
pub struct RegistryImagePuller {
    client: Client,
    staging_dir: PathBuf,
}

impl RegistryImagePuller {
    pub fn new(staging_dir: PathBuf) -> Self {
        let config = oci_client::client::ClientConfig::default();
        Self {
            client: Client::new(config),
            staging_dir,
        }
    }
}
```

(Delete the `impl Default for RegistryImagePuller`.) Replace the body of `pull` with:

```rust
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError> {
        let reference: Reference = image
            .parse()
            .map_err(|e| OciError::Pull(format!("invalid image reference '{image}': {e}")))?;

        let (manifest, manifest_digest, config_json) = self
            .client
            .pull_manifest_and_config(&reference, &auth)
            .await
            .map_err(|e| OciError::Pull(format!("pull failed: {e}")))?;

        let config: super::config::OciImageConfig =
            serde_json::from_str(&config_json).map_err(OciError::Json)?;

        let staging = tempfile::TempDir::new_in(&self.staging_dir).map_err(OciError::Io)?;

        let mut layers = Vec::with_capacity(manifest.layers.len());
        for (index, desc) in manifest.layers.iter().enumerate() {
            let compression = match desc.media_type.as_str() {
                t if t.contains("gzip") || t.contains("+gzip") => LayerCompression::Gzip,
                t if t.contains("zstd") || t.contains("+zstd") => LayerCompression::Zstd,
                _ => LayerCompression::None,
            };

            let layer_path = staging.path().join(format!("layer-{index}"));
            let file = tokio::fs::File::create(&layer_path)
                .await
                .map_err(OciError::Io)?;
            self.client
                .pull_blob(&reference, desc, file)
                .await
                .map_err(|e| OciError::Pull(format!("layer pull/verify failed: {e}")))?;

            layers.push(LayerBlob {
                digest: desc.digest.clone(),
                compression,
                path: layer_path,
            });
        }

        Ok(PulledImage {
            digest: manifest_digest,
            config,
            layers,
            _staging: Some(staging),
        })
    }
```

Remove the now-unused imports (`sha2`, `LayerBlob`/`Sha256` if unused) per the compiler. Keep `LayerBlob`, `LayerCompression`, `OciError`, `OciImagePuller`, `PulledImage` imports as needed.

- [ ] **Step 7: Wire the staging dir in `src/artifacts/acquirer.rs`**

Replace `ArtifactAcquirer::new` (lines 80-86) with:

```rust
    pub fn new(config: AppConfig) -> Self {
        let staging_dir = config.artifact_dir.clone();
        Self {
            config,
            puller: Arc::new(RegistryImagePuller::new(staging_dir)),
            unpacker: Arc::new(TarRootfsUnpacker::new()),
        }
    }
```

Update any test mocks / other `RegistryImagePuller::new(...)` or `LayerBlob { .. }` / `PulledImage { .. }` sites found in Step 1 to the new shapes.

- [ ] **Step 8: Build and run the full suite**

Run: `cargo build && cargo test`
Expected: PASS. Unpack tests now read layers from files; layout reads verify digests.

- [ ] **Step 9: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml src/oci/mod.rs src/oci/unpack.rs src/oci/layout.rs src/oci/registry.rs src/artifacts/acquirer.rs
git commit -m "refactor(oci): stream layers to disk instead of buffering in RAM"
```

---

### Task 3: Add a layout digest-mismatch test

**Files:**
- Modify: `src/oci/layout.rs` (test module — create `#[cfg(test)]` if absent)

- [ ] **Step 1: Write a failing test for tampered-blob rejection**

Add a test that builds a minimal OCI layout dir (an `index.json` pointing at a manifest blob, a manifest with one layer descriptor, the config blob, and a layer blob whose bytes do NOT match the descriptor digest), then asserts `read_oci_layout` returns `OciError::DigestMismatch`. Use `tempfile::tempdir()` for the layout root and write blobs under `blobs/sha256/<hex>`.

- [ ] **Step 2: Run it**

Run: `cargo test --lib oci::layout -- --nocapture`
Expected: PASS (the verify-first logic from Task 2 already enforces this — this test pins the behavior).

- [ ] **Step 3: Commit**

```bash
git add src/oci/layout.rs
git commit -m "test(oci): assert layout read rejects digest-mismatched blob"
```

---

### Task 4: Record ADR-015

**Files:**
- Create: `docs/adr/015-streaming-oci-layer-staging.md`
- Modify: `docs/adr/011-inprocess-oci-acquisition.md` (cross-reference)

- [ ] **Step 1: Write the ADR**

Document: status Proposed, date 2026-05-27. Context = ADR-011 consequence #3 (whole image in RAM). Decision = stage layers to disk, verify-before-extract on both paths, `tempfile` RAII cleanup, registry uses `pull_manifest_and_config` + `pull_blob`. Consequences = bounded RAM, transient disk ≈ compressed image size under `artifact_dir`, `tempfile` now a runtime dep. Reference the spec. Add an "Amended by ADR-015" note to ADR-011's References section.

- [ ] **Step 2: Commit**

```bash
git add docs/adr/015-streaming-oci-layer-staging.md docs/adr/011-inprocess-oci-acquisition.md
git commit -m "docs(adr): ADR-015 streaming OCI layer staging"
```

- [ ] **Step 3: Refresh the GitNexus index**

Run: `npx gitnexus analyze`
Expected: index updated (code changed across `src/oci/` and `src/artifacts/`).
