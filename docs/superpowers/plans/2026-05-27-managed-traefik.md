# Denia-Managed Traefik Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Denia pull, configure, run, and supervise its own Traefik edge as a host child process — no operator-installed Traefik, no opt-out flag.

**Architecture:** Denia pulls the official Traefik OCI image in-process (reusing `oci-client` + `TarRootfsUnpacker`), execs the statically-linked `traefik` binary directly on the host (no namespaces) so it binds `:80`/`:443`, generates Traefik's static config (entrypoints + file provider + `le` ACME HTTP-01) plus the existing dynamic config, and runs a supervisor task with crash-restart backoff and graceful shutdown. Managed Traefik is unconditional.

**Tech Stack:** Rust 2024, axum, tokio (process/signal), `oci-client`, `tempfile`, existing `src/oci` + `src/ingress` modules.

**Spec:** `docs/superpowers/specs/2026-05-27-managed-traefik-design.md`

---

## File Structure

- `src/config.rs` (modify) — new `AppConfig` fields (`traefik_image`, `acme_email`, `http_port`, `https_port`, derived `traefik_dir`), dynamic-config path default moves under `traefik_dir/dynamic`, new `ConfigError::AcmeEmailRequired` + `require_acme_email` helper.
- `src/oci/pull_to_dir.rs` (create) — `pull_image_to_dir(...)` helper: cache-check, pull, atomic unpack, binary verify, digest write. Exposed via `src/oci/mod.rs`.
- `src/ingress/traefik_supervisor.rs` (create) — `TraefikStaticOptions` + `render_static_config` (pure), backoff + spawn-error classification helpers (pure), `TraefikSpawner` trait + real impl, `TraefikSupervisor` run loop.
- `src/ingress/mod.rs` (modify) — register the module.
- `src/main.rs` (modify) — spawn the supervisor task with shutdown wiring.
- `docs/adr/016-managed-traefik.md` (create) — ADR.
- `README.md`, `AGENTS.md` (modify) — env vars + Traefik convention.

Each task is TDD where the logic is pure/fakeable; process-spawn behavior is tested behind the `TraefikSpawner` trait. Run `cargo fmt --all` before each commit.

---

## Task 1: Config fields, path default, ACME-email validation

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing tests**

Add to a test module in `src/config.rs`:

```rust
#[cfg(test)]
mod managed_traefik_tests {
    use super::*;

    fn base() -> AppConfig {
        AppConfig::for_test("0123456789012345678901234567890123")
    }

    #[test]
    fn traefik_dir_under_data_dir() {
        let c = base();
        assert_eq!(c.traefik_dir, c.data_dir.join("traefik"));
    }

    #[test]
    fn dynamic_config_defaults_under_traefik_dir() {
        // When DENIA_TRAEFIK_DYNAMIC_CONFIG is unset, path is traefik_dir/dynamic/denia.yml
        let c = base();
        assert_eq!(
            c.traefik_dynamic_config_path,
            c.data_dir.join("traefik/dynamic/denia.yml")
        );
    }

    #[test]
    fn defaults_for_ports_and_image() {
        let c = base();
        assert_eq!(c.http_port, 80);
        assert_eq!(c.https_port, 443);
        assert!(c.traefik_image.starts_with("docker.io/library/traefik:"));
        assert!(c.acme_email.is_none());
    }

    #[test]
    fn require_acme_email_errors_when_tls_used_without_email() {
        let c = base();
        assert!(matches!(
            c.require_acme_email(true),
            Err(ConfigError::AcmeEmailRequired)
        ));
    }

    #[test]
    fn require_acme_email_ok_when_no_tls() {
        let c = base();
        assert!(c.require_acme_email(false).is_ok());
    }

    #[test]
    fn require_acme_email_ok_when_email_present() {
        let mut c = base();
        c.acme_email = Some("ops@example.com".into());
        assert!(c.require_acme_email(true).is_ok());
    }
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib config::managed_traefik_tests`
Expected: FAIL — fields/variant/method do not exist.

- [ ] **Step 3: Implement**

In `src/config.rs`:

1. Add fields to `AppConfig`:
```rust
    pub traefik_image: String,
    pub acme_email: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub traefik_dir: PathBuf,
```

2. Add `ConfigError` variant:
```rust
    #[error("DENIA_ACME_EMAIL must be set when any service uses TLS")]
    AcmeEmailRequired,
```

3. In `from_env`, after `data_dir` is computed and BEFORE building `Self`:
```rust
        let traefik_dir = data_dir.join("traefik");
        let traefik_image = env::var("DENIA_TRAEFIK_IMAGE")
            .unwrap_or_else(|_| "docker.io/library/traefik:v3.3".to_string());
        let acme_email = env::var("DENIA_ACME_EMAIL").ok().filter(|v| !v.is_empty());
        let http_port = env::var("DENIA_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80);
        let https_port = env::var("DENIA_HTTPS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(443);
```

4. Change the `traefik_dynamic_config_path` default to depend on `traefik_dir`:
```rust
        let traefik_dynamic_config_path = env::var("DENIA_TRAEFIK_DYNAMIC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| traefik_dir.join("dynamic/denia.yml"));
```

5. Add the new fields to the returned `Self { .. }` in `from_env`.

6. Add the same fields to `for_test` (use `data_dir.join("traefik")` for `traefik_dir`, `traefik_dir.join("dynamic/denia.yml")` for the dynamic path, `http_port: 80`, `https_port: 443`, `acme_email: None`, `traefik_image: "docker.io/library/traefik:v3.3".into()`). NOTE: `for_test` currently sets `traefik_dynamic_config_path` to `/tmp/denia-traefik.yml`; the test above asserts the `from_env`-style default, so in `for_test` set `traefik_dynamic_config_path: data_dir.join("traefik/dynamic/denia.yml")` and `traefik_dir: data_dir.join("traefik")`.

7. Add the helper method on `impl AppConfig`:
```rust
    pub fn require_acme_email(&self, tls_in_use: bool) -> Result<(), ConfigError> {
        if tls_in_use && self.acme_email.is_none() {
            return Err(ConfigError::AcmeEmailRequired);
        }
        Ok(())
    }
```

- [ ] **Step 4: Run tests, verify they pass + check no `for_test` assertion regressions**

Run: `cargo test --lib config::managed_traefik_tests` → PASS.
Then run the **full** suite: `cargo test`. The `for_test` change moves
`traefik_dynamic_config_path` from `/tmp/denia-traefik.yml` to
`data_dir.join("traefik/dynamic/denia.yml")`; any existing test that asserts the
old value will now fail. Search for dependents and fix their expectations:
`grep -rn "denia-traefik.yml\|traefik_dynamic_config_path" src tests`.
Expected: full suite PASS after fixing any dependents.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/config.rs
git commit -m "feat(config): add managed-traefik config + acme-email validation"
```

---

## Task 2: Static config renderer

**Files:**
- Create: `src/ingress/traefik_supervisor.rs`
- Modify: `src/ingress/mod.rs`
- Test: inline in `src/ingress/traefik_supervisor.rs`

- [ ] **Step 1: Write failing test**

Create `src/ingress/traefik_supervisor.rs`:

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TraefikStaticOptions {
    pub http_port: u16,
    pub https_port: u16,
    pub dynamic_dir: PathBuf,
    pub acme_email: String,
    pub acme_storage: PathBuf,
    pub resolver: String,
}

pub fn render_static_config(opts: &TraefikStaticOptions) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> TraefikStaticOptions {
        TraefikStaticOptions {
            http_port: 80,
            https_port: 443,
            dynamic_dir: PathBuf::from("/var/lib/denia/traefik/dynamic"),
            acme_email: "ops@example.com".into(),
            acme_storage: PathBuf::from("/var/lib/denia/traefik/acme.json"),
            resolver: "le".into(),
        }
    }

    #[test]
    fn renders_entrypoints_and_file_provider() {
        let y = render_static_config(&opts());
        assert!(y.contains("address: \":80\""));
        assert!(y.contains("address: \":443\""));
        assert!(y.contains("directory: \"/var/lib/denia/traefik/dynamic\""));
        assert!(y.contains("watch: true"));
    }

    #[test]
    fn renders_acme_resolver() {
        let y = render_static_config(&opts());
        assert!(y.contains("le:"));
        assert!(y.contains("email: \"ops@example.com\""));
        assert!(y.contains("storage: \"/var/lib/denia/traefik/acme.json\""));
        assert!(y.contains("entryPoint: web"));
    }
}
```

Add to `src/ingress/mod.rs`:
```rust
pub mod traefik_supervisor;
```
(Do NOT add a glob `pub use` to avoid name clashes with `traefik::*`.)

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: FAIL — `todo!()` panics.

- [ ] **Step 3: Implement `render_static_config`**

```rust
pub fn render_static_config(opts: &TraefikStaticOptions) -> String {
    format!(
        "entryPoints:\n\
\x20 web:\n\
\x20   address: \":{http}\"\n\
\x20 websecure:\n\
\x20   address: \":{https}\"\n\
providers:\n\
\x20 file:\n\
\x20   directory: \"{dir}\"\n\
\x20   watch: true\n\
certificatesResolvers:\n\
\x20 {resolver}:\n\
\x20   acme:\n\
\x20     email: \"{email}\"\n\
\x20     storage: \"{storage}\"\n\
\x20     httpChallenge:\n\
\x20       entryPoint: web\n",
        http = opts.http_port,
        https = opts.https_port,
        dir = opts.dynamic_dir.display(),
        resolver = opts.resolver,
        email = opts.acme_email,
        storage = opts.acme_storage.display(),
    )
}
```

- [ ] **Step 4: Run test, verify it passes**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/ingress/traefik_supervisor.rs src/ingress/mod.rs
git commit -m "feat(ingress): render Traefik static config"
```

---

## Task 3: OCI pull-to-dir helper (atomic, cached, verified)

**Files:**
- Create: `src/oci/pull_to_dir.rs`
- Modify: `src/oci/mod.rs`
- Test: inline in `src/oci/pull_to_dir.rs`

- [ ] **Step 1: Write failing test**

Create `src/oci/pull_to_dir.rs`:

```rust
use std::path::Path;

use async_trait::async_trait;

use super::{LayerBlob, OciError, OciImagePuller, OciRootfsUnpacker, PulledImage, RegistryAuth};

/// Pull `image`, unpack into `<traefik_dir>/rootfs` atomically, verify the binary
/// at `binary_rel` exists, and record the digest in `<traefik_dir>/.image-digest`.
/// Returns the image digest. Skips work if the cached digest matches the pinned
/// digest in `image` and the binary is already present.
pub async fn pull_image_to_dir(
    puller: &dyn OciImagePuller,
    unpacker: &dyn OciRootfsUnpacker,
    image: &str,
    auth: RegistryAuth,
    traefik_dir: &Path,
    binary_rel: &str,
) -> Result<String, OciError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakePuller {
        digest: String,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl OciImagePuller for FakePuller {
        async fn pull(&self, _image: &str, _auth: RegistryAuth) -> Result<PulledImage, OciError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(PulledImage {
                digest: self.digest.clone(),
                config: serde_json::from_str("{}").unwrap(),
                layers: vec![LayerBlob {
                    digest: "sha256:layer".into(),
                    compression: super::super::LayerCompression::None,
                    path: PathBuf::from("/dev/null"),
                }],
                _staging: None,
            })
        }
        async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
            unreachable!()
        }
    }

    // Unpacker that drops the traefik binary into the target rootfs.
    struct FakeUnpacker;
    impl OciRootfsUnpacker for FakeUnpacker {
        fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
            let bin = rootfs_dir.join("usr/local/bin");
            fs::create_dir_all(&bin)?;
            fs::write(bin.join("traefik"), b"#!/bin/true\n")?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn pulls_unpacks_and_records_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        let puller = FakePuller { digest: "sha256:abc".into(), calls: AtomicUsize::new(0) };
        let digest = pull_image_to_dir(
            &puller, &FakeUnpacker, "docker.io/library/traefik:v3.3",
            RegistryAuth::Anonymous, &dir, "usr/local/bin/traefik",
        ).await.unwrap();
        assert_eq!(digest, "sha256:abc");
        assert!(dir.join("rootfs/usr/local/bin/traefik").exists());
        assert_eq!(fs::read_to_string(dir.join(".image-digest")).unwrap(), "sha256:abc");
        assert_eq!(puller.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cache_hit_skips_pull_for_pinned_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        // Pre-seed cache + binary.
        fs::create_dir_all(dir.join("rootfs/usr/local/bin")).unwrap();
        fs::write(dir.join("rootfs/usr/local/bin/traefik"), b"x").unwrap();
        fs::write(dir.join(".image-digest"), "sha256:pinned").unwrap();
        let puller = FakePuller { digest: "sha256:pinned".into(), calls: AtomicUsize::new(0) };
        let digest = pull_image_to_dir(
            &puller, &FakeUnpacker,
            "docker.io/library/traefik@sha256:pinned",
            RegistryAuth::Anonymous, &dir, "usr/local/bin/traefik",
        ).await.unwrap();
        assert_eq!(digest, "sha256:pinned");
        assert_eq!(puller.calls.load(Ordering::SeqCst), 0, "must skip pull on cache hit");
    }

    #[tokio::test]
    async fn missing_binary_after_unpack_errors() {
        struct EmptyUnpacker;
        impl OciRootfsUnpacker for EmptyUnpacker {
            fn unpack(&self, _l: &[LayerBlob], _r: &Path) -> Result<(), OciError> { Ok(()) }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        let puller = FakePuller { digest: "sha256:x".into(), calls: AtomicUsize::new(0) };
        let err = pull_image_to_dir(
            &puller, &EmptyUnpacker, "docker.io/library/traefik:v3.3",
            RegistryAuth::Anonymous, &dir, "usr/local/bin/traefik",
        ).await;
        assert!(err.is_err());
        assert!(!dir.join(".image-digest").exists(), "digest must not be written on failure");
    }
}
```

Add to `src/oci/mod.rs`:
```rust
pub mod pull_to_dir;
pub use pull_to_dir::pull_image_to_dir;
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --lib oci::pull_to_dir`
Expected: FAIL — `todo!()`.

- [ ] **Step 3: Implement**

```rust
fn pinned_digest(image: &str) -> Option<&str> {
    image.split_once("@").map(|(_, d)| d)
}

pub async fn pull_image_to_dir(
    puller: &dyn OciImagePuller,
    unpacker: &dyn OciRootfsUnpacker,
    image: &str,
    auth: RegistryAuth,
    traefik_dir: &Path,
    binary_rel: &str,
) -> Result<String, OciError> {
    std::fs::create_dir_all(traefik_dir)?;
    let rootfs = traefik_dir.join("rootfs");
    let digest_file = traefik_dir.join(".image-digest");
    let binary = rootfs.join(binary_rel);

    // Cache hit: pinned digest matches recorded digest and binary present.
    if let Some(pinned) = pinned_digest(image) {
        if binary.exists() {
            if let Ok(cached) = std::fs::read_to_string(&digest_file) {
                if cached.trim() == pinned {
                    return Ok(pinned.to_string());
                }
            }
        }
    }

    let pulled = puller.pull(image, auth).await?;

    // Unpack into a temp dir, then atomically swap into place.
    let staging = tempfile::TempDir::new_in(traefik_dir).map_err(OciError::Io)?;
    let staged_rootfs = staging.path().join("rootfs");
    unpacker.unpack(&pulled.layers, &staged_rootfs)?;

    if !staged_rootfs.join(binary_rel).exists() {
        return Err(OciError::Pull(format!(
            "traefik binary missing at {binary_rel} after unpack"
        )));
    }

    if rootfs.exists() {
        std::fs::remove_dir_all(&rootfs)?;
    }
    std::fs::rename(&staged_rootfs, &rootfs)?;
    std::fs::write(&digest_file, &pulled.digest)?;

    Ok(pulled.digest)
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test --lib oci::pull_to_dir`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/oci/pull_to_dir.rs src/oci/mod.rs
git commit -m "feat(oci): pull_image_to_dir with atomic unpack + digest cache"
```

---

## Task 4: Backoff + spawn-error classification helpers

**Files:**
- Modify: `src/ingress/traefik_supervisor.rs`
- Test: inline

- [ ] **Step 1: Write failing tests**

Append to `src/ingress/traefik_supervisor.rs`:

```rust
use std::time::Duration;

pub fn backoff_delay(attempt: u32) -> Duration {
    todo!()
}

/// A spawn/exit failure that must NOT be retried (port already bound).
pub fn is_fatal_bind_error(err: &std::io::Error) -> bool {
    todo!()
}
```

Add tests in the existing `mod tests`:

```rust
    #[test]
    fn backoff_is_capped_and_monotonic() {
        assert_eq!(super::backoff_delay(0), std::time::Duration::from_secs(1));
        assert!(super::backoff_delay(1) >= std::time::Duration::from_secs(2));
        // capped at 30s
        assert_eq!(super::backoff_delay(20), std::time::Duration::from_secs(30));
    }

    #[test]
    fn addr_in_use_is_fatal() {
        let e = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert!(super::is_fatal_bind_error(&e));
        let other = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!super::is_fatal_bind_error(&other));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt).unwrap_or(u64::MAX).min(30);
    Duration::from_secs(secs)
}

pub fn is_fatal_bind_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::AddrInUse
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/ingress/traefik_supervisor.rs
git commit -m "feat(ingress): traefik supervisor backoff + bind-error classification"
```

---

## Task 5: Supervisor run loop (behind a spawner trait)

**Files:**
- Modify: `src/ingress/traefik_supervisor.rs`
- Test: inline

This task adds the async loop. The child process is abstracted behind `TraefikSpawner` so the loop is testable without binding ports or pulling images. The real spawner is added but its end-to-end behavior is validated manually (Task 6 + integration).

- [ ] **Step 0: Enable tokio `test-util` (required for `start_paused`)**

The supervisor tests use `#[tokio::test(start_paused = true)]`, which is gated
behind tokio's `test-util` feature. `Cargo.toml` does not enable it. Add it to
the tokio dependency features (alongside the existing
`fs, io-util, macros, net, process, rt-multi-thread, signal, time`):

```toml
tokio = { version = "1", features = ["fs", "io-util", "macros", "net", "process", "rt-multi-thread", "signal", "time", "test-util"] }
```

Run `cargo build` to confirm it resolves. Commit this with the task's final
commit.

- [ ] **Step 1: Write failing test**

Append:

```rust
use tokio::sync::mpsc;

#[derive(Debug, PartialEq, Eq)]
pub enum ChildExit {
    Exited,          // process ended; restart
    FatalBind,       // EADDRINUSE; stop
}

#[async_trait::async_trait]
pub trait TraefikSpawner: Send + Sync {
    /// Spawn the child and resolve when it exits (or fails to bind).
    async fn run_once(&self) -> ChildExit;
}

pub struct TraefikSupervisor<S: TraefikSpawner> {
    pub spawner: S,
    pub max_restarts_for_test: Option<u32>, // None = unbounded (prod)
}

impl<S: TraefikSpawner> TraefikSupervisor<S> {
    /// Loop: run child, restart on exit with backoff, stop on FatalBind or shutdown.
    pub async fn run(self, mut shutdown: mpsc::Receiver<()>) -> SupervisorOutcome {
        todo!()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorOutcome {
    Shutdown,
    FatalBind,
    MaxRestarts, // test-only guard tripped
}
```

Tests:

```rust
    struct ExitOnceThenFatal { calls: std::sync::Mutex<u32> }
    #[async_trait::async_trait]
    impl super::TraefikSpawner for ExitOnceThenFatal {
        async fn run_once(&self) -> super::ChildExit {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c == 1 { super::ChildExit::Exited } else { super::ChildExit::FatalBind }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_then_stops_on_fatal_bind() {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let sup = super::TraefikSupervisor {
            spawner: ExitOnceThenFatal { calls: std::sync::Mutex::new(0) },
            max_restarts_for_test: Some(5),
        };
        let outcome = sup.run(rx).await;
        assert_eq!(outcome, super::SupervisorOutcome::FatalBind);
    }

    struct AlwaysExit;
    #[async_trait::async_trait]
    impl super::TraefikSpawner for AlwaysExit {
        async fn run_once(&self) -> super::ChildExit { super::ChildExit::Exited }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_loop() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let sup = super::TraefikSupervisor { spawner: AlwaysExit, max_restarts_for_test: None };
        let handle = tokio::spawn(sup.run(rx));
        tx.send(()).await.unwrap();
        let outcome = handle.await.unwrap();
        assert_eq!(outcome, super::SupervisorOutcome::Shutdown);
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: FAIL.

- [ ] **Step 3: Implement the loop**

```rust
impl<S: TraefikSpawner> TraefikSupervisor<S> {
    pub async fn run(self, mut shutdown: mpsc::Receiver<()>) -> SupervisorOutcome {
        let mut attempt: u32 = 0;
        let mut restarts: u32 = 0;
        loop {
            tokio::select! {
                _ = shutdown.recv() => return SupervisorOutcome::Shutdown,
                exit = self.spawner.run_once() => {
                    match exit {
                        ChildExit::FatalBind => return SupervisorOutcome::FatalBind,
                        ChildExit::Exited => {
                            restarts += 1;
                            if let Some(max) = self.max_restarts_for_test {
                                if restarts >= max { return SupervisorOutcome::MaxRestarts; }
                            }
                            let delay = backoff_delay(attempt);
                            attempt = attempt.saturating_add(1);
                            tokio::select! {
                                _ = shutdown.recv() => return SupervisorOutcome::Shutdown,
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}
```

NOTE: keep `start_paused = true` so `sleep` auto-advances in tests.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --lib traefik_supervisor::tests`
Expected: PASS.

- [ ] **Step 5: Add the real spawner + acquire/config writer (no unit test; compiled + manual)**

Append a production `HostTraefikSpawner` and an `acquire_and_prepare` function. These call OS/process APIs; they are exercised manually in Task 6.

```rust
use crate::config::AppConfig;
use crate::oci::{pull_image_to_dir, OciImagePuller, OciRootfsUnpacker, RegistryAuth};
use std::os::unix::fs::PermissionsExt;

/// Pull/cache the image, write static config, ensure dynamic dir + acme.json.
/// Returns the path to the traefik binary.
pub async fn acquire_and_prepare(
    config: &AppConfig,
    puller: &dyn OciImagePuller,
    unpacker: &dyn OciRootfsUnpacker,
) -> std::io::Result<PathBuf> {
    let dir = &config.traefik_dir;
    pull_image_to_dir(
        puller, unpacker, &config.traefik_image, RegistryAuth::Anonymous,
        dir, "usr/local/bin/traefik",
    )
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    let dynamic_dir = dir.join("dynamic");
    std::fs::create_dir_all(&dynamic_dir)?;
    let denia_yml = dynamic_dir.join("denia.yml");
    if !denia_yml.exists() {
        std::fs::write(&denia_yml, "http:\n  routers: {}\n  services: {}\n")?;
    }

    let acme = dir.join("acme.json");
    if !acme.exists() {
        std::fs::write(&acme, b"")?;
        std::fs::set_permissions(&acme, std::fs::Permissions::from_mode(0o600))?;
    }

    let static_opts = TraefikStaticOptions {
        http_port: config.http_port,
        https_port: config.https_port,
        dynamic_dir,
        acme_email: config.acme_email.clone().unwrap_or_default(),
        acme_storage: acme,
        resolver: config.ingress_resolver_name(),
    };
    std::fs::write(dir.join("traefik.yml"), render_static_config(&static_opts))?;

    Ok(dir.join("rootfs/usr/local/bin/traefik"))
}
```

Add a small accessor on `AppConfig` in `src/config.rs` returning the resolver name (reuse `acme_resolver`): `pub fn ingress_resolver_name(&self) -> String { self.acme_resolver.clone() }`. (If `acme_resolver` already exists on `AppConfig` — it does — just clone it; no new field.)

Also add the real spawner:

```rust
pub struct HostTraefikSpawner {
    pub binary: PathBuf,
    pub config_file: PathBuf,
    pub cwd: PathBuf,
    pub log_path: PathBuf,
}

#[async_trait::async_trait]
impl TraefikSpawner for HostTraefikSpawner {
    async fn run_once(&self) -> ChildExit {
        use std::process::Stdio;
        let log = match std::fs::OpenOptions::new().create(true).append(true).open(&self.log_path) {
            Ok(f) => f,
            Err(_) => return ChildExit::Exited,
        };
        let log_err = match log.try_clone() { Ok(f) => f, Err(_) => return ChildExit::Exited };
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg(format!("--configfile={}", self.config_file.display()))
            .current_dir(&self.cwd)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        match cmd.spawn() {
            Ok(mut child) => {
                let _ = child.wait().await;
                ChildExit::Exited
            }
            Err(e) if is_fatal_bind_error(&e) => ChildExit::FatalBind,
            Err(_) => ChildExit::Exited,
        }
    }
}
```

NOTE (coverage honesty): `EADDRINUSE` for a *bound* port surfaces from Traefik's
own **exit**, not from the `spawn()` syscall. So `is_fatal_bind_error` only
catches the (rare) case where `spawn()` itself fails with `AddrInUse`. In
practice a port conflict yields `ChildExit::Exited` → restart loop, with the
conflict logged to `traefik.log` until the operator stops the other Traefik. The
unit tests in Step 1 use a fake spawner that returns `FatalBind` directly — they
verify the *loop's* fatal-stop branch, NOT that the production spawner detects a
real port conflict. This v1 gap is intentional and documented in ADR-016;
precise detection (scan child log for the bind error → return `FatalBind`) is a
follow-up. Do not claim the supervisor tests cover real EADDRINUSE behavior.

- [ ] **Step 6: Build + commit**

Run: `cargo build`
Expected: compiles.

```bash
cargo fmt --all
git add src/ingress/traefik_supervisor.rs src/config.rs Cargo.toml Cargo.lock
git commit -m "feat(ingress): traefik supervisor loop + host spawner + acquire"
```

---

## Task 6: Wire the supervisor into `main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Implement wiring**

In `src/main.rs`:

1. **Fail-fast ACME-email check (spec goal).** After `store.migrate()?` and after
   `AppState` is available, compute whether any service uses TLS and validate the
   email. The simplest place is right before building the router / serving, using
   the already-built `AppState` (`state.services`). Restructure so `AppState` is
   built into a `state` binding you can read before `serve`:

```rust
    let state = AppState::new(config.clone(), &store);
    let tls_in_use = state
        .services
        .list_services()?
        .iter()
        .any(|s| s.tls_enabled);
    state.config.require_acme_email(tls_in_use)?; // ConfigError -> anyhow via `?`
```

   (Keep one owned `config` for the supervisor task below; `AppState::new` takes
   `config` by value, so pass `config.clone()` to it and keep the original for the
   task, or read `state.config` — both work since `AppConfig: Clone`.)

2. **Spawn the supervisor.** After the scheduler task is spawned and before
   `axum::serve`, add an `mpsc` shutdown channel and spawn the supervisor. Ensure
   `traefik_dir` exists before constructing the puller (its `staging_dir` is
   `traefik_dir`, and `RegistryImagePuller::pull` does `TempDir::new_in(staging_dir)`):

```rust
    use denia::ingress::traefik_supervisor::{
        acquire_and_prepare, HostTraefikSpawner, TraefikSupervisor,
    };
    use denia::oci::registry::RegistryImagePuller;
    use denia::oci::unpack::TarRootfsUnpacker;

    let (traefik_shutdown_tx, traefik_shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let traefik_task = {
        let config = config.clone();
        tokio::spawn(async move {
            // Ensure staging/traefik dir exists before the puller creates a TempDir in it.
            if let Err(e) = std::fs::create_dir_all(&config.traefik_dir) {
                eprintln!("traefik dir create failed: {e}");
                return;
            }
            let puller = RegistryImagePuller::new(config.traefik_dir.clone());
            let unpacker = TarRootfsUnpacker::new();
            match acquire_and_prepare(&config, &puller, &unpacker).await {
                Ok(binary) => {
                    let spawner = HostTraefikSpawner {
                        binary,
                        config_file: config.traefik_dir.join("traefik.yml"),
                        cwd: config.traefik_dir.clone(),
                        log_path: config.log_dir.join("traefik.log"),
                    };
                    let sup = TraefikSupervisor { spawner, max_restarts_for_test: None };
                    let outcome = sup.run(traefik_shutdown_rx).await;
                    eprintln!("traefik supervisor exited: {outcome:?}");
                }
                Err(e) => eprintln!("traefik acquire failed: {e}"),
            }
        })
    };

    let app = build_router(state);
```

After `axum::serve(...).await?` returns (graceful shutdown), signal + await the task:

```rust
    let _ = traefik_shutdown_tx.send(()).await;
    let _ = traefik_task.await;
```

Note the existing scheduler shutdown uses a `oneshot`; the new traefik channel is
a separate `mpsc` — no conflict. `RegistryImagePuller`/`TarRootfsUnpacker`/
`traefik_supervisor` are reachable from the crate root (`pub mod` in `src/oci`
and `src/ingress`; `src/lib.rs` exposes both). `acquire_and_prepare`,
`HostTraefikSpawner`, `TraefikSupervisor` must be `pub` in
`src/ingress/traefik_supervisor.rs` (they are, per Task 5).

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles. Fix visibility (`pub`) issues if the new items aren't reachable from `main.rs`.

- [ ] **Step 3: Full test + lint**

Run:
```bash
cargo test
cargo fmt --all
cargo clippy --all-targets --all-features
```
Expected: tests pass, no clippy errors.

- [ ] **Step 4: Manual smoke (host with cgroup v2 + outbound network)**

```bash
export DENIA_ADMIN_TOKEN=$(head -c48 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c40)
export DENIA_ACME_EMAIL=ops@example.com
sudo -E cargo run
```
Expected: logs show image pull, `traefik.yml` written under `data_dir/traefik`, Traefik bound to `:80`. Check `data_dir/traefik/traefik.yml`, `acme.json` (mode 0600), and `logs/traefik.log`. Stop with Ctrl-C; confirm the child is terminated.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/lib.rs
git commit -m "feat: supervise managed Traefik from main"
```

---

## Task 7: ADR-016 + docs

**Files:**
- Create: `docs/adr/016-managed-traefik.md`
- Modify: `docs/adr/README.md` (add the index row), `README.md`, `AGENTS.md`

- [ ] **Step 1: Write ADR-016**

Follow the format of `docs/adr/015-streaming-oci-layer-staging.md`. Status: Accepted. Capture: context (operator-installed Traefik is friction), decision (Denia pulls + supervises its own Traefik host process, unconditional, no opt-out; ACME HTTP-01 `le`, email required when TLS used; dynamic path under `data_dir/traefik/dynamic`), consequences (operator must not run a separate Traefik; port conflict shows as restart loop + log until resolved — follow-up to make fatal via log scan; restart drops in-flight conns; SELinux/log-rotation noted), references to the spec and ADR-005/007/011/015.

- [ ] **Step 2: Update README**

- Add to the env table: `DENIA_TRAEFIK_IMAGE`, `DENIA_ACME_EMAIL`, `DENIA_HTTP_PORT`, `DENIA_HTTPS_PORT`.
- Note the dynamic-config default moved to `<data_dir>/traefik/dynamic/denia.yml`.
- Add a "Managed Traefik" subsection + an **Upgrade note**: stop any external Traefik; Denia now owns `:80`/`:443`.

- [ ] **Step 3: Update AGENTS.md**

Replace the "Traefik integration uses the file provider..." project-convention line to state Denia now **runs and supervises** its own Traefik (OCI-pulled host process), still via the file provider, and owns the loopback bridges.

- [ ] **Step 4: Commit**

```bash
git add docs/adr/016-managed-traefik.md docs/adr/README.md README.md AGENTS.md
git commit -m "docs(adr): ADR-016 managed Traefik + README/AGENTS updates"
```

---

## Final Verification

- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] Manual smoke (Task 6 Step 4) on a cgroup-v2 host with outbound network.
- [ ] Report exact commands + results before finishing (per AGENTS.md).

## Out of Scope (follow-ups)

- Precise `EADDRINUSE` → fatal via child-log scan (v1 logs + restart-loops instead).
- `traefik.log` rotation.
- cgroup confinement of the Traefik child.
- DNS-01 / multi-resolver.
- TODO #11 install.sh (separate spec) — now without a Traefik install step.
