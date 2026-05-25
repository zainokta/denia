# In-Process OCI Image Acquisition (Replace skopeo + umoci) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the external `skopeo` (registry copy + config inspect) and `umoci` (rootfs unpack) host binaries with pure-Rust, in-process equivalents, removing two host dependencies from the deploy path.

**Architecture:** Introduce a `src/oci/` module exposing two traits — `OciImagePuller` (pull manifest/config/layers from a registry, plus read a local OCI layout produced by BuildKit) and `OciRootfsUnpacker` (apply ordered layer tarballs onto a rootfs, with whiteout + path-traversal safety). `ArtifactAcquirer` is rewired to call these traits instead of `CommandRunner.run("skopeo"|"umoci")`. Existing post-unpack rootfs ownership adjustment stays in place until the rustix syscall refactor replaces `chown`. Registry auth is handled by a `RegistryCredentialProvider` abstraction with Anonymous/Basic (ghcr, Docker Hub, private) in core and ECR/GAR token-exchange providers behind cargo features.

**Tech Stack:** `oci-client` (OCI distribution pull), `oci-spec` (manifest/index/config types), `tar` + `flate2` + `zstd` (layer extraction), `sha2` (digest verify), `aws-config`/`aws-sdk-ecr` (feature `ecr`), `gcp_auth` (feature `gar`).

---

## Context

### Why this change

`ArtifactAcquirer` (`src/artifacts/acquirer.rs`) currently shells out to two Go binaries via `CommandRunner`:

- `skopeo copy docker://<image> oci:<artifact_dir>` — pull external image into a local OCI layout (`acquire_external_image`, lines 187–202).
- `skopeo inspect --config oci:<artifact_dir>` — read image config JSON for `Entrypoint`/`Cmd`/`Env`/`WorkingDir` (`inspect_image_config`, lines 226–236).
- `umoci unpack --image oci:<artifact_dir> <bundle>` — extract layers into a rootfs bundle (`materialize_rootfs_bundle`, lines 204–224).

ADR-003 (`docs/adr/003-linux-runtime-process-runner.md`, lines 24, 66) records this as a host requirement and lists it as a Negative consequence ("Hosts must provide `skopeo` and `umoci`"). These are exactly the dependencies the user wants to eliminate.

**What `skopeo`/`umoci` do:** `skopeo` is a daemonless CLI that moves OCI/Docker images between a registry and local storage and reads image metadata. `umoci` applies image layers onto a filesystem tree (the rootfs the `LinuxRuntime` runs). Both are battle-tested precisely because the hard parts — registry auth per provider, layer media types, OCI whiteout semantics, and safe tar extraction — are subtle.

**Outcome:** External-image deploys pull and unpack entirely in-process. `buildctl` (BuildKit), `sops`, `unshare`, `setpriv`, `chown`, and `traefik` remain host dependencies (out of scope for this plan). The local OCI layout written by BuildKit (`type=oci,dest=<artifact_dir>`, `acquirer.rs:166-169`) is still consumed, so the unpacker must also read an on-disk OCI layout, not only registry-pulled blobs.

### Scope decisions (confirmed with user)

- Replace **both** `skopeo` and `umoci` via Rust crates (Route A).
- Target registries: **ghcr.io, Docker Hub, private/self-hosted (Basic), AWS ECR, GCP Artifact Registry**.
- ECR and GAR auth go behind cargo features (`ecr`, `gar`) so the core build stays lean; default features enable neither.

### Per AGENTS.md

This is a dependency change → **must amend ADR-003** (Task 12). Host root stays the trust boundary; tar extraction is a security boundary and must reject path traversal / unsafe links.

> **Spec:** `docs/superpowers/specs/2026-05-25-inprocess-oci-acquisition.md`

---

## File Structure

**New module `src/oci/`:**

- `src/oci/mod.rs` — module exports; `OciImagePuller`, `OciRootfsUnpacker` traits; `PulledImage`, `LayerBlob`, `OciError`.
- `src/oci/unpack.rs` — `TarRootfsUnpacker`: apply ordered (gzip/zstd) tar layers to a rootfs dir; whiteout + opaque-dir handling; path-traversal/symlink safety.
- `src/oci/registry.rs` — `RegistryImagePuller`: pull manifest/config/layers via `oci-client`; host-platform selection from manifest index, explicit platform override only; sha256 verify.
- `src/oci/layout.rs` — read a BuildKit on-disk OCI layout (`index.json` → manifest → config + layer blob paths).
- `src/oci/config.rs` — relocate `OciImageConfig`/`OciImageProcessConfig`/`RootfsBundleSpec` here (currently in `acquirer.rs:10-32, 239-266`); re-export from `acquirer` to limit churn.
- `src/oci/credentials.rs` — `RegistryCredentialProvider` trait → `oci_client::secrets::RegistryAuth`; `StaticCredentialProvider` (Anonymous/Basic from SOPS).
- `src/oci/ecr.rs` (feature `ecr`) — `EcrCredentialProvider`: `aws-sdk-ecr` `get_authorization_token` → Basic auth.
- `src/oci/gar.rs` (feature `gar`) — `GarCredentialProvider`: `gcp_auth` access token → Basic auth (`oauth2accesstoken`).

**Modified:**

- `Cargo.toml` — add deps + `[features]`.
- `src/lib.rs:14` area — add `pub mod oci;`.
- `src/artifacts/acquirer.rs` — replace skopeo/umoci runner calls with trait calls; `ArtifactAcquirer` holds `Arc<dyn OciImagePuller>` + `Arc<dyn OciRootfsUnpacker>`.
- `src/config.rs:47-52, 67-68, 85-86` — remove `registry_pull_binary`/`oci_unpack_binary` (and their env vars `DENIA_REGISTRY_PULL_BINARY`/`DENIA_OCI_UNPACK_BINARY`); add optional registry-credential config (SOPS secret ref + provider selector).
- `src/app.rs` — construct the concrete puller/unpacker and inject into `ArtifactAcquirer`.
- `tests/backend_contract.rs`, `tests/deploy_orchestration.rs` — replace `FakeCommandRunner` skopeo/umoci expectations with fake puller/unpacker.
- `docs/adr/003-linux-runtime-process-runner.md`, `README.md:29,56,97-98`, `AGENTS.md` references — doc updates.

**New tests:**

- `tests/oci_unpack.rs` — unpack unit/integration: layer application, whiteouts, opaque dirs, traversal rejection, symlink escape rejection, gzip + zstd.
- `tests/oci_registry_live.rs` — **opt-in** real pull from a public `ghcr.io` image, gated behind `DENIA_RUN_NETWORK_TESTS=1` and `#[ignore]` (mirrors the privileged-test gate in `tests/linux_runtime_privileged.rs`).

---

## Crate API reference (verify versions with `cargo add` / context7 at execution)

- `oci-client`: `Client::pull(&Reference, &RegistryAuth, Vec<&str> accepted_media_types) -> ImageData`. `ImageData { layers: Vec<ImageLayer { data, media_type, .. }>, config: Config { data, media_type, .. }, digest, .. }`. Handles bearer-token flow. Confirm whether it performs manifest-index platform selection itself; Denia must enforce host-platform match or explicit override. `RegistryAuth::{Anonymous, Basic(user, pass)}`. `Reference::try_from("ghcr.io/acme/web:latest")`.
- `oci-spec::image`: `ImageConfiguration`, `ImageManifest`, `ImageIndex`, `OciLayout` for parsing on-disk layout.
- `tar::Archive`, `flate2::read::GzDecoder`, `zstd::stream::read::Decoder`, `sha2::Sha256`.
- Whiteout markers (OCI image-spec): file prefix `.wh.<name>` deletes `<name>`; `.wh..wh..opq` in a dir clears all prior entries of that dir.

> Use context7 (`mcp__plugin_context7_context7`) for current `oci-client`/`oci-spec` API at execution time — versions move.

---

## Task 1: Add dependencies and module scaffold

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs` (add `pub mod oci;` after line 11 `pub mod metrics;`)
- Create: `src/oci/mod.rs`

- [ ] **Step 1: Add deps + features to `Cargo.toml`**

```toml
[dependencies]
# ... existing ...
oci-client = "0.15"
oci-spec = "0.8"
tar = "0.4"
flate2 = "1"
zstd = "0.13"
sha2 = "0.10"
aws-config = { version = "1", optional = true }
aws-sdk-ecr = { version = "1", optional = true }
gcp_auth = { version = "0.12", optional = true }

[features]
default = []
ecr = ["dep:aws-config", "dep:aws-sdk-ecr"]
gar = ["dep:gcp_auth"]
```

- [ ] **Step 2: Create `src/oci/mod.rs` with traits + shared types**

```rust
pub mod config;
pub mod credentials;
pub mod layout;
pub mod registry;
pub mod unpack;
#[cfg(feature = "ecr")]
pub mod ecr;
#[cfg(feature = "gar")]
pub mod gar;

use async_trait::async_trait;
use thiserror::Error;

pub use config::{OciImageConfig, OciImageProcessConfig, RootfsBundleSpec};

#[derive(Debug, Clone)]
pub enum LayerCompression { Gzip, Zstd, None }

#[derive(Debug, Clone)]
pub struct LayerBlob {
    pub digest: String,
    pub compression: LayerCompression,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PulledImage {
    pub digest: String,
    pub config: OciImageConfig,
    pub layers: Vec<LayerBlob>,
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("registry pull failed: {0}")]
    Pull(String),
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("unsafe path in layer: {0}")]
    UnsafePath(String),
    #[error("oci layout error: {0}")]
    Layout(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[async_trait]
pub trait OciImagePuller: Send + Sync {
    /// Pull an external image reference from a registry.
    async fn pull(&self, image: &str) -> Result<PulledImage, OciError>;
    /// Read an on-disk OCI layout (BuildKit output) at `layout_dir`.
    async fn read_layout(&self, layout_dir: &std::path::Path) -> Result<PulledImage, OciError>;
}

pub trait OciRootfsUnpacker: Send + Sync {
    /// Apply ordered layers onto `rootfs_dir`, creating it if needed.
    fn unpack(&self, layers: &[LayerBlob], rootfs_dir: &std::path::Path) -> Result<(), OciError>;
}
```

- [ ] **Step 3: `cargo build`**

Run: `cargo build`
Expected: PASS (empty submodules will fail until created — create empty stub files returning `unimplemented!()` if needed to compile incrementally, or implement Task 2 next). If splitting, create stub `config.rs` etc. with minimal types first.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/oci/mod.rs
git commit -m "feat(oci): add in-process OCI module scaffold and deps"
```

---

## Task 2: Relocate image-config types to `src/oci/config.rs`

Move `OciImageConfig`, `OciImageProcessConfig`, `RootfsBundleSpec`, `default_workdir`, and the `TryFrom<OciImageConfig> for RootfsBundleSpec` impl out of `acquirer.rs` into `oci/config.rs`. Re-export from `acquirer` so existing imports keep working.

**Files:**
- Create: `src/oci/config.rs`
- Modify: `src/artifacts/acquirer.rs:10-32, 239-283`

- [ ] **Step 1: Move types verbatim** into `oci/config.rs`. The `TryFrom` error type currently is `ArtifactAcquireError`; change `RootfsBundleSpec::try_from` to return a local `ConfigError` (`MissingProcessArgv`, `InvalidEnvironmentEntry`) and add `#[from]` conversion in `ArtifactAcquireError`.

- [ ] **Step 2: Re-export in acquirer:** `use crate::oci::config::{OciImageConfig, RootfsBundleSpec};` and delete the moved definitions.

- [ ] **Step 3: Run** `cargo build` then `cargo test --lib` — Expected: PASS (pure move, behavior identical).

- [ ] **Step 4: Commit** — `refactor(oci): relocate image config types into oci module`

---

## Task 3: Fake puller/unpacker for tests

**Files:**
- Modify: `src/oci/mod.rs` (add `#[cfg(test)]`-friendly fakes, or a `pub mod fakes;`)
- Create: `src/oci/fakes.rs`

- [ ] **Step 1: Implement `FakePuller` + `FakeUnpacker`** recording calls and returning queued `PulledImage` / capturing the layer count + target dir. Mirror the `FakeCommandRunner` pattern in `src/command.rs:70-111` (Arc<Mutex<..>>, `calls()` accessor).

- [ ] **Step 2: Build** — `cargo build` — Expected: PASS.

- [ ] **Step 3: Commit** — `test(oci): add fake puller and unpacker`

---

## Task 4: `TarRootfsUnpacker` — the safety-critical core

This is the highest-risk task (replaces `umoci`). Whiteouts and traversal safety must be test-driven first.

**Files:**
- Create: `src/oci/unpack.rs`
- Test: `tests/oci_unpack.rs`

- [ ] **Step 1: Write failing tests** in `tests/oci_unpack.rs` covering, each as its own `#[test]`:
  1. Single gzip layer extracts files into `rootfs/`.
  2. Second layer overwrites a file from the first.
  3. `.wh.foo` marker in layer 2 deletes `foo` created in layer 1.
  4. `.wh..wh..opq` in a dir clears prior dir contents before applying layer 2 entries.
  5. zstd-compressed layer extracts correctly.
  6. **Security:** tar entry with path `../escape` is rejected with `OciError::UnsafePath` and nothing is written outside `rootfs`.
  7. **Security:** absolute path `/etc/passwd` entry is rejected with `OciError::UnsafePath` and nothing is written to host.
  8. **Security:** a symlink `link -> /etc` followed by an entry `link/evil` does not write through the symlink outside `rootfs`.

  Build test layers as in-memory tarballs with the `tar` crate + `flate2`/`zstd` encoders. Example for case 6:

```rust
fn gz_layer(entries: &[(&str, &[u8])]) -> denia::oci::LayerBlob {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, data) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, path, *data).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    use std::io::Write; enc.write_all(&raw).unwrap();
    denia::oci::LayerBlob { digest: "sha256:test".into(), compression: denia::oci::LayerCompression::Gzip, data: enc.finish().unwrap() }
}

#[test]
fn rejects_parent_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let rootfs = dir.path().join("rootfs");
    let layer = gz_layer(&[("../escape", b"x")]);
    let err = denia::oci::unpack::TarRootfsUnpacker.unpack(&[layer], &rootfs).unwrap_err();
    assert!(matches!(err, denia::oci::OciError::UnsafePath(_)));
    assert!(!dir.path().join("escape").exists());
}
```

- [ ] **Step 2: Run tests, verify they fail** — `cargo test --test oci_unpack` — Expected: FAIL (unpacker unimplemented).

- [ ] **Step 3: Implement `TarRootfsUnpacker`.** Core algorithm per layer, in order:
  - Decompress by `compression` (`GzDecoder` / `zstd::Decoder` / passthrough).
  - Iterate `tar::Archive::entries()`. For each entry:
    - Compute the entry path; **reject** if it contains a `..` component or is absolute after normalization that escapes root (resolve against `rootfs` and verify the canonical parent stays within `rootfs`).
    - Handle whiteouts by **basename**: `.wh..wh..opq` → remove all existing entries in the entry's parent dir; `.wh.<name>` → remove `<parent>/<name>`; otherwise extract.
    - Before writing through any path, ensure no path component is an existing symlink that points outside `rootfs` (do not follow symlinks when resolving the target path — build the destination by joining components and `lstat`-checking).
    - Extract regular files/dirs/symlinks/hardlinks; set the link target literally (do not canonicalize). Preserve mode.

  Provide a `fn safe_join(root: &Path, entry: &Path) -> Result<PathBuf, OciError>` helper that returns `UnsafePath` on traversal. This is the security boundary — keep it small and well-tested.

- [ ] **Step 4: Run tests** — `cargo test --test oci_unpack` — Expected: PASS (all 8).

- [ ] **Step 5: Commit** — `feat(oci): add safe tar rootfs unpacker with whiteout handling`

---

## Task 5: `RegistryImagePuller` via oci-client

**Files:**
- Create: `src/oci/registry.rs`

- [ ] **Step 1: Implement `RegistryImagePuller`** holding an `oci_client::Client` and an `Arc<dyn RegistryCredentialProvider>`. `pull(image)`:
  - Parse `Reference::try_from(image)`.
  - Resolve auth via provider keyed on `reference.registry()`.
  - `client.pull(&reference, &auth, accepted_media_types)` with both Docker + OCI layer media types accepted.
  - Map each `ImageLayer` → `LayerBlob` (infer `LayerCompression` from media type: `*.tar.gzip`/`+gzip` → Gzip, `+zstd` → Zstd, else None).
  - Parse `ImageData.config.data` as `OciImageConfig` (serde; the config JSON has the `config.{Entrypoint,Cmd,Env,WorkingDir}` shape already modeled in `oci/config.rs`).
  - Verify each layer's sha256 against its descriptor digest; return `DigestMismatch` on failure. (`oci-client` may already verify; verify explicitly with `sha2` as defense-in-depth.)
  - Return `PulledImage { digest, config, layers }`.

- [ ] **Step 2: Unit test** with a mocked client is impractical (oci-client has no fake); rely on the **live test in Task 11** plus a small parse test: feed a captured config JSON blob and assert `OciImageConfig` deserializes + `RootfsBundleSpec::try_from` yields expected argv. Write that as `tests/oci_unpack.rs` sibling or an inline `#[cfg(test)]` mod.

- [ ] **Step 3: Build** — `cargo build` — Expected: PASS.

- [ ] **Step 4: Commit** — `feat(oci): add registry image puller`

---

## Task 6: On-disk OCI layout reader (BuildKit path)

**Files:**
- Create: `src/oci/layout.rs`; wire `read_layout` impl into `RegistryImagePuller` (or a separate `LayoutReader` the puller delegates to).

- [ ] **Step 1: Implement `read_layout(layout_dir)`:**
  - Parse `oci-layout` + `index.json` (`oci_spec::image::ImageIndex`).
  - Select the manifest descriptor (single image; if index has platforms pick host arch/`linux/amd64` fallback). Read manifest blob `blobs/sha256/<hex>`.
  - Read config blob → `OciImageConfig`. Read each layer blob → `LayerBlob` (compression from media type).
  - Return `PulledImage`.

- [ ] **Step 2: Test** by building a tiny OCI layout on disk in a tempdir (write `oci-layout`, `index.json`, manifest, config, one gzip layer blob with matching sha256) and asserting `read_layout` returns the layer + parsed config. Add to `tests/oci_unpack.rs`.

- [ ] **Step 3: Run** — `cargo test --test oci_unpack` — Expected: PASS.

- [ ] **Step 4: Commit** — `feat(oci): read on-disk OCI layout from buildkit output`

---

## Task 7: Credential provider + SOPS-sourced Basic auth

**Files:**
- Create: `src/oci/credentials.rs`
- Reuse: `src/secrets.rs` (`SopsSecretStore::decrypt`, `SecretRef`) for pulling registry username/password.

- [ ] **Step 1: Define trait**

```rust
#[async_trait]
pub trait RegistryCredentialProvider: Send + Sync {
    async fn auth_for(&self, registry: &str) -> Result<oci_client::secrets::RegistryAuth, OciError>;
}
```

- [ ] **Step 2: `StaticCredentialProvider`** — map of `registry -> RegistryAuth`; default `Anonymous` for unknown hosts (public ghcr/Docker Hub pulls work anonymously). Build it from a SOPS secret (JSON `{registry, username, password}` list) decrypted via `SopsSecretStore` at startup. Reuse `SecretRef::parse` for the secret name.

- [ ] **Step 3: Test** the static map: known host → Basic, unknown → Anonymous (no network).

- [ ] **Step 4: Commit** — `feat(oci): add registry credential provider with SOPS basic auth`

---

## Task 8: ECR credential provider (feature `ecr`)

**Files:**
- Create: `src/oci/ecr.rs` (`#![cfg(feature = "ecr")]`)

- [ ] **Step 1: Implement `EcrCredentialProvider`** — on `auth_for` for a `*.dkr.ecr.<region>.amazonaws.com` host: call `aws_sdk_ecr::Client::get_authorization_token`, base64-decode the token → `AWS:<password>`, return `RegistryAuth::Basic("AWS", password)`. Cache token until near its `expires_at`. Load AWS config via `aws_config::load_defaults` (env/instance role).

- [ ] **Step 2: Test** — token-string decode helper (`AWS:secret` split) unit test, no live AWS call.

- [ ] **Step 3: Build with feature** — `cargo build --features ecr` — Expected: PASS.

- [ ] **Step 4: Commit** — `feat(oci): add ECR credential provider behind feature`

---

## Task 9: GAR credential provider (feature `gar`)

**Files:**
- Create: `src/oci/gar.rs` (`#![cfg(feature = "gar")]`)

- [ ] **Step 1: Implement `GarCredentialProvider`** — for `*-docker.pkg.dev` / `gcr.io` hosts: obtain a Google access token via `gcp_auth` (ADC / service account), return `RegistryAuth::Basic("oauth2accesstoken", access_token)`. Cache until expiry.

- [ ] **Step 2: Build with feature** — `cargo build --features gar` — Expected: PASS.

- [ ] **Step 3: Commit** — `feat(oci): add GAR credential provider behind feature`

---

## Task 10: Rewire `ArtifactAcquirer` to traits

**Files:**
- Modify: `src/artifacts/acquirer.rs:63-237`
- Modify: `src/config.rs` (remove the two binary fields + env vars)
- Modify: `src/app.rs` (construct + inject puller/unpacker)

- [ ] **Step 1:** Change `ArtifactAcquirer` to hold `puller: Arc<dyn OciImagePuller>` and `unpacker: Arc<dyn OciRootfsUnpacker>` alongside `config`. Update `new` signature.

- [ ] **Step 2:** Rewrite the acquire methods:
  - `acquire_external_image` → `self.puller.pull(image).await?`; digest from `PulledImage.digest`. Store the `PulledImage` (or its layers) so unpack can use them — adjust `acquire`/`acquire_rootfs_bundle_*` to thread the pulled image through rather than re-reading a layout. Simplest: have `acquire` return `(ArtifactRecord, PulledImage)` internally, or store layers in a temp keyed by digest.
  - Git path: after `buildctl` writes the OCI layout (still via `CommandRunner`), call `self.puller.read_layout(&self.config.artifact_dir).await?`.
  - `materialize_rootfs_bundle` → `self.unpacker.unpack(&pulled.layers, &bundle_dir.join("rootfs"))?` instead of `umoci`. Drop the `runner.run("umoci"...)` call. Keep writing `process.json`.
  - Preserve the runtime-security rootfs ownership step after unpack: run the existing `chown -R --no-dereference {userns_base}:{userns_base} <rootfs>` via `CommandRunner` if that code exists at execution time. The later rustix syscall refactor may replace it with `recursive_lchown`; this plan must not remove it.
  - `inspect_image_config` → use `pulled.config` directly; delete the `skopeo inspect` call.
  - Keep `CommandRunner` only for the BuildKit (`buildctl`) invocation in `acquire_git`.

- [ ] **Step 3:** Remove `registry_pull_binary` + `oci_unpack_binary` from `AppConfig` (`config.rs:47-52, 67-68, 85-86`) and their `DENIA_REGISTRY_PULL_BINARY` / `DENIA_OCI_UNPACK_BINARY` env reads. Update `for_test`.

- [ ] **Step 4:** In `src/app.rs`, build `RegistryImagePuller` (with credential provider) + `TarRootfsUnpacker`, wrap in `Arc`, pass to `ArtifactAcquirer::new`.

- [ ] **Step 5:** `cargo build` — Expected: PASS. (Test fixes in Task 11.)

- [ ] **Step 6: Commit** — `feat(acquirer): use in-process OCI pull and unpack`

---

## Task 11: Update existing tests + add live pull test

**Files:**
- Modify: `tests/backend_contract.rs` (lines ~221-225, 318-327, 400-405, 545, 759-768, 871-876)
- Modify: `tests/deploy_orchestration.rs` (lines ~665-674)
- Create: `tests/oci_registry_live.rs`

- [ ] **Step 1:** Replace the `FakeCommandRunner` queued-output + `assert_eq!(commands, ["skopeo copy ...", "umoci unpack ...", "skopeo inspect ..."])` assertions with injection of `FakePuller`/`FakeUnpacker` (Task 3). Assert the acquirer called `pull(image)` / `read_layout` and `unpack` with expected layer count + bundle path, and that `process.json` is written from the fake's returned config. Drop `config.oci_unpack_binary` assertion at `backend_contract.rs:545`.

- [ ] **Step 2:** Keep `FakeCommandRunner` only where `buildctl` (Git build) is exercised; assert the `buildctl ...` command still runs for the Git path.

- [ ] **Step 3:** Add `tests/oci_registry_live.rs`: `#[ignore]` test gated on `std::env::var("DENIA_RUN_NETWORK_TESTS").is_ok()` that pulls a small public image (e.g. `ghcr.io`/`docker.io` public test image), unpacks to a tempdir, and asserts `rootfs` is non-empty and `process.json`-derivable config has argv. Mirror the gating comment style in `tests/linux_runtime_privileged.rs`.

- [ ] **Step 4: Run** — `cargo test` — Expected: PASS (live test skipped by default). Then `DENIA_RUN_NETWORK_TESTS=1 cargo test --test oci_registry_live -- --ignored` in a network-allowed env.

- [ ] **Step 5: Commit** — `test: cover in-process OCI acquisition; drop skopeo/umoci command tests`

---

## Task 12: ADR amend + docs

**Files:**
- Modify: `docs/adr/003-linux-runtime-process-runner.md` (Decision lines 24, Consequences 66; add an amendment note)
- Modify: `README.md:29, 56, 97-98`
- Modify: `AGENTS.md` references section + `docs/adr/README.md` index if needed

- [ ] **Step 1:** Amend ADR-003: record that external-image pull and rootfs unpack are now in-process (`oci-client` + tar), removing the `skopeo`/`umoci` host requirement; note registries supported and ECR/GAR feature gates; note tar extraction is the new security boundary. Prefer an ADR-003 amendment since the runtime contract is unchanged. Do not reuse ADR-005; it already records runtime security hardening.

- [ ] **Step 2:** README: drop `skopeo`/`umoci` from host prerequisites (line 56); update the OCI pull description (line 29); remove the `DENIA_REGISTRY_PULL_BINARY` / `DENIA_OCI_UNPACK_BINARY` env rows (lines 97-98); document any new registry-credential SOPS secret + feature flags.

- [ ] **Step 3: Commit** — `docs: record in-process OCI acquisition (ADR-003 amendment)`

---

## Verification

End-to-end, in order:

1. `cargo fmt --all`
2. `cargo build` — core build, no `skopeo`/`umoci` references remain (`grep -rn "skopeo\|umoci" src/` returns nothing). If runtime-security hardening has landed, verify `chown`/ownership behavior still runs after in-process unpack.
3. `cargo build --features ecr,gar` — feature builds compile.
4. `cargo clippy --all-targets --all-features` — no new warnings.
5. `cargo test` — full suite green; live network test skipped by default.
6. `DENIA_RUN_NETWORK_TESTS=1 cargo test --test oci_registry_live -- --ignored` (network-allowed env) — real public pull + unpack succeeds, rootfs populated.
7. Privileged path unchanged: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` still passes (rootfs bundle now produced in-process feeds the same `LinuxRuntime`).
8. GitNexus: `gitnexus_impact` on `ArtifactAcquirer`/`materialize_rootfs_bundle` before editing (Task 10), `gitnexus_detect_changes` before each commit, re-`npx gitnexus analyze` after.

## Risks / watch-points

- **Tar extraction safety (Task 4)** is the security-critical surface — traversal, absolute paths, and symlink-escape must all be covered by tests before implementation is trusted.
- **`oci-client` API drift** — confirm exact `pull` signature + `ImageData` shape via context7 at execution.
- **Threading the `PulledImage` through `acquirer`** (Task 10 Step 2) is the main structural change; the current code re-reads a layout between pull and unpack — collapsing that needs care so the Git (BuildKit→layout) and external-image (registry→blobs) paths converge cleanly on the unpacker.
- **ECR/GAR token caching** — providers must cache and refresh on expiry to avoid per-pull auth calls.
