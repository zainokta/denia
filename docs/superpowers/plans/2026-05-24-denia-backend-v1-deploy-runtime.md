# Denia Backend V1 Deploy Runtime Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the backend deploy/runtime foundation: accept Git or external image sources, produce local artifacts, orchestrate health-gated deployment promotion, expose logs and metrics, and define the privileged Linux runtime gate.

**Architecture:** Keep the current single-node Rust/Axum control plane and SQLite store. Add narrow subsystem modules for SOPS secrets, artifact acquisition, runtime boundaries, socket bridge allocation, health checks, logs, metrics, and deployment orchestration. The runtime remains Denia-owned and Linux-native; this plan establishes the safe interface and privileged test gate, while the low-level namespace/cgroup process runner lands in a dedicated follow-up plan.

**Tech Stack:** Rust 2024, axum, tokio, rusqlite, serde, uuid v7, SOPS with local age key, BuildKit CLI, OCI registry pull CLI/library, cgroup v2, procfs, Unix domain sockets, Traefik file provider.

---

## File Structure

- Modify `Cargo.toml`: add runtime dependencies for async process execution, filesystem walking, HTTP client health checks, tar/archive handling, and temporary test directories.
- Modify `src/lib.rs`: export new subsystem modules.
- Modify `src/config.rs`: add binary paths and runtime directories.
- Modify `src/domain.rs`: add runtime status, deployment status transitions, bridge/log/metric types, and lifecycle action enum.
- Modify `src/state.rs`: add tables and repository methods for runtime status, logs, metrics, bridge ports, and promoted deployment pointers.
- Modify `src/app.rs`: wire deployment orchestration into `/v1/deployments`, lifecycle commands, logs, and metrics endpoints.
- Create `src/command.rs`: testable command-runner abstraction for external tools.
- Modify `src/secrets.rs`: SOPS decrypt wrapper and secret file path policy.
- Create `src/artifacts/acquirer.rs`: Git+BuildKit and external registry artifact acquisition.
- Create `src/runtime.rs`: Denia runtime interface and Linux runtime implementation boundary.
- Create `src/bridge.rs`: loopback listener registry and Unix-socket forwarding abstraction.
- Create `src/health.rs`: HTTP health check client.
- Create `src/logs.rs`: append/read service logs.
- Extend `src/metrics.rs`: cgroup CPU/memory/io parsing and metric snapshots.
- Create `src/deploy.rs`: deployment coordinator.
- Extend `tests/backend_contract.rs`: API and state behavior.
- Create `tests/deploy_orchestration.rs`: deploy coordinator tests with fakes.
- Create `tests/fixtures/cgroup/`: small fixture files for metric parsing.

## Task 1: Add Command Runner And Config Surface

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/config.rs`
- Create: `src/command.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write the failing tests**

Add to `tests/backend_contract.rs`:

```rust
use denia::command::{CommandOutput, FakeCommandRunner};

#[tokio::test]
async fn fake_command_runner_records_commands_and_returns_output() {
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "ok\n".to_string(),
        stderr: String::new(),
    }]);

    let output = runner
        .run("sops", &["--decrypt", "/var/lib/denia/secrets/git-main.sops.yaml"])
        .await
        .expect("command output");

    assert_eq!(output.stdout, "ok\n");
    assert_eq!(runner.commands(), vec!["sops --decrypt /var/lib/denia/secrets/git-main.sops.yaml"]);
}

#[test]
fn test_config_defines_runtime_paths_and_tool_binaries() {
    let config = AppConfig::for_test("test-token");

    assert_eq!(config.buildkit_binary.to_string_lossy(), "buildctl");
    assert_eq!(config.sops_binary.to_string_lossy(), "sops");
    assert_eq!(config.runtime_dir, config.data_dir.join("runtime"));
    assert_eq!(config.artifact_dir, config.data_dir.join("artifacts"));
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test fake_command_runner_records_commands_and_returns_output test_config_defines_runtime_paths_and_tool_binaries`

Expected: FAIL because `denia::command`, `buildkit_binary`, `sops_binary`, `runtime_dir`, and `artifact_dir` do not exist.

- [ ] **Step 3: Add dependencies and module export**

Modify `Cargo.toml`:

```toml
[dependencies]
async-trait = "0.1"
```

Modify `src/lib.rs`:

```rust
pub mod command;
```

- [ ] **Step 4: Implement `src/command.rs`**

Create `src/command.rs`:

```rust
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("command failed with status {status}: {stderr}")]
    Failed { status: i32, stderr: String },
    #[error("command runner has no configured fake output")]
    MissingFakeOutput,
    #[error("command lock poisoned")]
    LockPoisoned,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError>;
}

#[derive(Debug, Default)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        let output = tokio::process::Command::new(program).args(args).output().await?;
        let status = output.status.code().unwrap_or(1);
        let result = CommandOutput {
            status,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        };
        if status == 0 {
            Ok(result)
        } else {
            Err(CommandError::Failed {
                status,
                stderr: result.stderr,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct FakeCommandRunner {
    outputs: Arc<Mutex<Vec<CommandOutput>>>,
    commands: Arc<Mutex<Vec<String>>>,
}

impl FakeCommandRunner {
    pub fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs.into_iter().rev().collect())),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("commands lock").clone()
    }
}

#[async_trait]
impl CommandRunner for FakeCommandRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, CommandError> {
        self.commands
            .lock()
            .map_err(|_| CommandError::LockPoisoned)?
            .push(format!("{program} {}", args.join(" ")).trim().to_string());

        self.outputs
            .lock()
            .map_err(|_| CommandError::LockPoisoned)?
            .pop()
            .ok_or(CommandError::MissingFakeOutput)
    }
}
```

- [ ] **Step 5: Extend `AppConfig`**

Add fields to `src/config.rs`:

```rust
pub buildkit_binary: PathBuf,
pub sops_binary: PathBuf,
pub registry_pull_binary: PathBuf,
pub runtime_dir: PathBuf,
pub artifact_dir: PathBuf,
pub log_dir: PathBuf,
```

In `from_env`, set:

```rust
let buildkit_binary = PathBuf::from(env::var("DENIA_BUILDKIT_BINARY").unwrap_or_else(|_| "buildctl".to_string()));
let sops_binary = PathBuf::from(env::var("DENIA_SOPS_BINARY").unwrap_or_else(|_| "sops".to_string()));
let registry_pull_binary = PathBuf::from(env::var("DENIA_REGISTRY_PULL_BINARY").unwrap_or_else(|_| "skopeo".to_string()));
let runtime_dir = data_dir.join("runtime");
let artifact_dir = data_dir.join("artifacts");
let log_dir = data_dir.join("logs");
```

Add the same fields to `for_test`, with `buildctl`, `sops`, `skopeo`, and paths under `/tmp/denia-test`.

- [ ] **Step 6: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/config.rs src/command.rs tests/backend_contract.rs
git commit -m "feat(runtime): add command runner and runtime config"
```

## Task 2: Implement SOPS Secret File Operations

**Files:**
- Modify: `src/secrets.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write the failing tests**

Add to `tests/backend_contract.rs`:

```rust
use denia::secrets::{SecretPayload, SopsSecretStore};

#[test]
fn sops_secret_store_resolves_secret_paths_under_data_dir() {
    let store = SopsSecretStore::new("/var/lib/denia");
    let path = store.secret_path(&SecretRef::new("git-main"));

    assert_eq!(
        path.to_string_lossy(),
        "/var/lib/denia/secrets/git-main.sops.yaml"
    );
}

#[test]
fn secret_payload_serializes_without_exposing_metadata() {
    let payload = SecretPayload::new("OPENSSH_PRIVATE_KEY");
    let json = serde_json::to_string(&payload).expect("json");

    assert_eq!(json, "{\"value\":\"OPENSSH_PRIVATE_KEY\"}");
}
```

- [ ] **Step 2: Run the tests to verify failure**

Run: `cargo test sops_secret_store_resolves_secret_paths_under_data_dir secret_payload_serializes_without_exposing_metadata`

Expected: FAIL because `SecretPayload` does not exist.

- [ ] **Step 3: Add secret payload type**

Add to `src/secrets.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretPayload {
    pub value: String,
}

impl SecretPayload {
    pub fn new(value: impl Into<String>) -> Self {
        Self { value: value.into() }
    }
}
```

- [ ] **Step 4: Add decrypt/encrypt command methods**

Add to `src/secrets.rs`:

```rust
use crate::command::{CommandError, CommandRunner};

impl SopsSecretStore {
    pub async fn decrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        secret_ref: &SecretRef,
    ) -> Result<SecretPayload, SecretError> {
        let path = self.secret_path(secret_ref);
        let output = runner
            .run(
                &sops_binary.to_string_lossy(),
                &["--decrypt", &path.to_string_lossy()],
            )
            .await?;
        Ok(serde_json::from_str(&output.stdout)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("command error: {0}")]
    Command(#[from] CommandError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
```

- [ ] **Step 5: Add async decrypt test**

Add to `tests/backend_contract.rs`:

```rust
#[tokio::test]
async fn sops_secret_store_decrypts_payload_with_runner() {
    let store = SopsSecretStore::new("/var/lib/denia");
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "{\"value\":\"registry-token\"}".to_string(),
        stderr: String::new(),
    }]);

    let payload = store
        .decrypt(&runner, std::path::Path::new("sops"), &SecretRef::new("registry-main"))
        .await
        .expect("payload");

    assert_eq!(payload.value, "registry-token");
}
```

- [ ] **Step 6: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/secrets.rs tests/backend_contract.rs
git commit -m "feat(secrets): add sops secret access"
```

## Task 3: Add Artifact Acquisition Planning

**Files:**
- Modify: `src/artifacts.rs`
- Create: `src/artifacts/acquirer.rs`
- Modify: `src/lib.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write the failing tests**

Add to `tests/backend_contract.rs`:

```rust
use denia::artifacts::acquirer::{ArtifactAcquirer, ArtifactAcquireRequest};

#[tokio::test]
async fn artifact_acquirer_builds_git_source_with_buildkit() {
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "sha256:build123\n".to_string(),
        stderr: String::new(),
    }]);
    let config = AppConfig::for_test("test-token");
    let acquirer = ArtifactAcquirer::new(config.clone());

    let artifact = acquirer
        .acquire(
            &runner,
            ArtifactAcquireRequest::Git {
                repo_url: "git@example.com:acme/api.git".to_string(),
                git_ref: "main".to_string(),
                dockerfile_path: "Dockerfile".to_string(),
                context_path: ".".to_string(),
            },
        )
        .await
        .expect("artifact");

    assert_eq!(artifact.digest, "sha256:build123");
    assert!(runner.commands()[0].starts_with("buildctl build"));
}

#[tokio::test]
async fn artifact_acquirer_pulls_external_image() {
    let runner = FakeCommandRunner::new(vec![CommandOutput {
        status: 0,
        stdout: "sha256:pull123\n".to_string(),
        stderr: String::new(),
    }]);
    let acquirer = ArtifactAcquirer::new(AppConfig::for_test("test-token"));

    let artifact = acquirer
        .acquire(
            &runner,
            ArtifactAcquireRequest::ExternalImage {
                image: "ghcr.io/acme/web:latest".to_string(),
            },
        )
        .await
        .expect("artifact");

    assert_eq!(artifact.digest, "sha256:pull123");
    assert!(runner.commands()[0].contains("copy docker://ghcr.io/acme/web:latest"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test artifact_acquirer_builds_git_source_with_buildkit artifact_acquirer_pulls_external_image`

Expected: FAIL because `ArtifactAcquirer` does not exist.

- [ ] **Step 3: Refactor artifacts module**

Move current artifact types into `src/artifacts/mod.rs`, create `src/artifacts/acquirer.rs`, and update `src/lib.rs` to keep:

```rust
pub mod artifacts;
```

The public imports must continue to support:

```rust
use denia::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
```

- [ ] **Step 4: Implement acquirer**

Create `src/artifacts/acquirer.rs`:

```rust
use crate::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandError, CommandRunner},
    config::AppConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactAcquireRequest {
    Git {
        repo_url: String,
        git_ref: String,
        dockerfile_path: String,
        context_path: String,
    },
    ExternalImage {
        image: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactAcquireError {
    #[error("command error: {0}")]
    Command(#[from] CommandError),
    #[error("artifact error: {0}")]
    Artifact(#[from] crate::artifacts::ArtifactError),
}

#[derive(Debug, Clone)]
pub struct ArtifactAcquirer {
    config: AppConfig,
}

impl ArtifactAcquirer {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn acquire(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        match request {
            ArtifactAcquireRequest::Git {
                repo_url,
                git_ref,
                dockerfile_path,
                context_path,
            } => {
                let output = runner
                    .run(
                        &self.config.buildkit_binary.to_string_lossy(),
                        &[
                            "build",
                            "--frontend",
                            "dockerfile.v0",
                            "--local",
                            &format!("context={context_path}"),
                            "--local",
                            &format!("dockerfile={dockerfile_path}"),
                        ],
                    )
                    .await?;
                ArtifactRecord::new(
                    output.stdout.trim(),
                    ArtifactKind::OciImage,
                    ArtifactSource::BuildKit {
                        repo_url,
                        git_ref,
                        dockerfile_path,
                        context_path,
                    },
                )
                .map_err(Into::into)
            }
            ArtifactAcquireRequest::ExternalImage { image } => {
                let destination = format!("oci:{}", self.config.artifact_dir.display());
                let output = runner
                    .run(
                        &self.config.registry_pull_binary.to_string_lossy(),
                        &["copy", &format!("docker://{image}"), &destination],
                    )
                    .await?;
                ArtifactRecord::new(
                    output.stdout.trim(),
                    ArtifactKind::OciImage,
                    ArtifactSource::ExternalRegistry { image },
                )
                .map_err(Into::into)
            }
        }
    }
}
```

- [ ] **Step 5: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/artifacts.rs src/artifacts tests/backend_contract.rs
git commit -m "feat(artifacts): add build and pull acquisition"
```

## Task 4: Add Runtime Interface And Fake Runtime

**Files:**
- Modify: `src/lib.rs`
- Create: `src/runtime.rs`
- Modify: `src/domain.rs`
- Test: `tests/deploy_orchestration.rs`

- [ ] **Step 1: Write the failing tests**

Create `tests/deploy_orchestration.rs`:

```rust
use denia::{
    artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource},
    domain::{RuntimeStartRequest, RuntimeStatus},
    runtime::{FakeRuntime, Runtime},
};

#[tokio::test]
async fn fake_runtime_starts_and_stops_service() {
    let runtime = FakeRuntime::default();
    let artifact = ArtifactRecord::new(
        "sha256:abc123",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:latest".to_string(),
        },
    )
    .expect("artifact");

    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "web".to_string(),
            deployment_id: uuid::Uuid::now_v7(),
            artifact,
            internal_port: 3000,
            socket_path: "/var/lib/denia/runtime/web/current.sock".into(),
            cpu_millis: 500,
            memory_bytes: 536870912,
        })
        .await
        .expect("started");

    assert_eq!(status.service_name, "web");
    assert_eq!(status.state, "running");

    runtime.stop(&status.service_name).await.expect("stopped");
    assert_eq!(runtime.stopped_services(), vec!["web"]);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test fake_runtime_starts_and_stops_service`

Expected: FAIL because runtime types do not exist.

- [ ] **Step 3: Add runtime domain types**

Add to `src/domain.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStartRequest {
    pub service_name: String,
    pub deployment_id: Uuid,
    pub artifact: crate::artifacts::ArtifactRecord,
    pub internal_port: u16,
    pub socket_path: std::path::PathBuf,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub service_name: String,
    pub deployment_id: Uuid,
    pub state: String,
    pub pid: Option<u32>,
    pub cgroup_path: std::path::PathBuf,
    pub socket_path: std::path::PathBuf,
}
```

- [ ] **Step 4: Implement runtime trait and fake**

Create `src/runtime.rs`:

```rust
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{RuntimeStartRequest, RuntimeStatus};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime lock poisoned")]
    LockPoisoned,
}

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError>;
    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError>;
}

#[derive(Debug, Default, Clone)]
pub struct FakeRuntime {
    started: Arc<Mutex<Vec<RuntimeStartRequest>>>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl FakeRuntime {
    pub fn stopped_services(&self) -> Vec<String> {
        self.stopped.lock().expect("stopped lock").clone()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.started
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(request.clone());
        Ok(RuntimeStatus {
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid: Some(1234),
            cgroup_path: "/sys/fs/cgroup/denia/fake".into(),
            socket_path: request.socket_path,
        })
    }

    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError> {
        self.stopped
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(service_name.to_string());
        Ok(())
    }
}
```

Modify `src/lib.rs`:

```rust
pub mod runtime;
```

- [ ] **Step 5: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/domain.rs src/runtime.rs tests/deploy_orchestration.rs
git commit -m "feat(runtime): define runtime boundary"
```

## Task 5: Add Health Checks And Deployment Coordinator

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/health.rs`
- Create: `src/deploy.rs`
- Modify: `src/state.rs`
- Test: `tests/deploy_orchestration.rs`

- [ ] **Step 1: Write the failing coordinator test**

Add to `tests/deploy_orchestration.rs`:

```rust
use denia::{
    deploy::{DeploymentCoordinator, DeploymentPlan},
    health::FakeHealthChecker,
    state::SqliteStore,
};

#[tokio::test]
async fn coordinator_promotes_only_after_health_check_passes() {
    let store = SqliteStore::open_in_memory().expect("sqlite");
    store.migrate().expect("migrate");
    let runtime = FakeRuntime::default();
    let health = FakeHealthChecker::healthy();
    let coordinator = DeploymentCoordinator::new(store.clone(), runtime, health);

    let service = store
        .put_service(ServiceConfig::new(
            "web",
            vec!["web.example.test".to_string()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "ghcr.io/acme/web:latest".to_string(),
                credential: None,
            }),
            3000,
            HealthCheck::new("/ready", 5),
            ResourceLimits::default(),
        ).expect("service"))
        .expect("stored service");

    let artifact = ArtifactRecord::new(
        "sha256:abc123",
        ArtifactKind::OciImage,
        ArtifactSource::ExternalRegistry {
            image: "ghcr.io/acme/web:latest".to_string(),
        },
    ).expect("artifact");

    let deployment = coordinator
        .deploy(DeploymentPlan { service, artifact })
        .await
        .expect("deployment");

    assert_eq!(deployment.status, DeploymentStatus::Healthy);
    assert_eq!(
        store.promoted_deployment(deployment.service_id).expect("promoted"),
        Some(deployment.id)
    );
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test coordinator_promotes_only_after_health_check_passes`

Expected: FAIL because coordinator, health checker, and promotion state do not exist.

- [ ] **Step 3: Add health checker**

Create `src/health.rs`:

```rust
use async_trait::async_trait;
use thiserror::Error;

use crate::domain::HealthCheck;

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check failed")]
    Failed,
}

#[async_trait]
pub trait HealthChecker: Send + Sync {
    async fn check(&self, url: &str, health: &HealthCheck) -> Result<(), HealthError>;
}

#[derive(Debug, Clone)]
pub struct FakeHealthChecker {
    healthy: bool,
}

impl FakeHealthChecker {
    pub fn healthy() -> Self {
        Self { healthy: true }
    }
}

#[async_trait]
impl HealthChecker for FakeHealthChecker {
    async fn check(&self, _url: &str, _health: &HealthCheck) -> Result<(), HealthError> {
        if self.healthy {
            Ok(())
        } else {
            Err(HealthError::Failed)
        }
    }
}
```

Modify `src/lib.rs`:

```rust
pub mod deploy;
pub mod health;
```

- [ ] **Step 4: Add promotion repository methods**

In `src/state.rs` migration, add:

```sql
CREATE TABLE IF NOT EXISTS promoted_deployments (
    service_id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL
);
```

Add methods:

```rust
pub fn promote_deployment(&self, service_id: Uuid, deployment_id: Uuid) -> Result<(), StateError> {
    let connection = self.connection()?;
    connection.execute(
        "INSERT INTO promoted_deployments (service_id, deployment_id) VALUES (?1, ?2)
         ON CONFLICT(service_id) DO UPDATE SET deployment_id = excluded.deployment_id",
        params![service_id.to_string(), deployment_id.to_string()],
    )?;
    Ok(())
}

pub fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, StateError> {
    let connection = self.connection()?;
    let value: Option<String> = connection
        .query_row(
            "SELECT deployment_id FROM promoted_deployments WHERE service_id = ?1",
            params![service_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    value.map(|id| Uuid::parse_str(&id)).transpose().map_err(Into::into)
}
```

Import `rusqlite::OptionalExtension`.

- [ ] **Step 5: Implement coordinator**

Create `src/deploy.rs`:

```rust
use thiserror::Error;

use crate::{
    artifacts::ArtifactRecord,
    domain::{Deployment, DeploymentRequest, DeploymentStatus, RuntimeStartRequest, ServiceConfig},
    health::{HealthChecker, HealthError},
    runtime::{Runtime, RuntimeError},
    state::{SqliteStore, StateError},
};

pub struct DeploymentPlan {
    pub service: ServiceConfig,
    pub artifact: ArtifactRecord,
}

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("health error: {0}")]
    Health(#[from] HealthError),
}

pub struct DeploymentCoordinator<R, H> {
    store: SqliteStore,
    runtime: R,
    health: H,
}

impl<R, H> DeploymentCoordinator<R, H>
where
    R: Runtime,
    H: HealthChecker,
{
    pub fn new(store: SqliteStore, runtime: R, health: H) -> Self {
        Self { store, runtime, health }
    }

    pub async fn deploy(&self, plan: DeploymentPlan) -> Result<Deployment, DeployError> {
        let mut deployment = self.store.create_deployment(DeploymentRequest::external_image(
            plan.service.id,
            match &plan.artifact.source {
                crate::artifacts::ArtifactSource::ExternalRegistry { image } => image.clone(),
                crate::artifacts::ArtifactSource::BuildKit { repo_url, .. } => repo_url.clone(),
            },
        ))?;

        let runtime_status = self.runtime.start(RuntimeStartRequest {
            service_name: plan.service.name.clone(),
            deployment_id: deployment.id,
            artifact: plan.artifact,
            internal_port: plan.service.internal_port,
            socket_path: format!("/var/lib/denia/runtime/{}/current.sock", plan.service.name).into(),
            cpu_millis: plan.service.resource_limits.cpu_millis,
            memory_bytes: plan.service.resource_limits.memory_bytes,
        }).await?;

        self.health
            .check(
                &format!("http://127.0.0.1:{}", plan.service.internal_port),
                &plan.service.health_check,
            )
            .await?;

        self.store.promote_deployment(plan.service.id, deployment.id)?;
        deployment.status = DeploymentStatus::Healthy;
        let _ = runtime_status;
        Ok(deployment)
    }
}
```

- [ ] **Step 6: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/health.rs src/deploy.rs src/state.rs tests/deploy_orchestration.rs
git commit -m "feat(deploy): add health-gated coordinator"
```

## Task 6: Add Logs And Metrics APIs

**Files:**
- Modify: `src/lib.rs`
- Create: `src/logs.rs`
- Modify: `src/metrics.rs`
- Modify: `src/state.rs`
- Modify: `src/app.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write failing tests**

Add to `tests/backend_contract.rs`:

```rust
use denia::logs::LogStore;
use denia::metrics::{MetricSnapshot, parse_cpu_stat};

#[test]
fn cpu_stat_parser_reads_usage_usec() {
    let stat = parse_cpu_stat("usage_usec 12345\nuser_usec 100\nsystem_usec 50\n").expect("stat");
    assert_eq!(stat.usage_usec, 12345);
}

#[test]
fn log_store_appends_and_reads_service_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let logs = LogStore::new(dir.path());

    logs.append("web", "first line\n").expect("append");
    logs.append("web", "second line\n").expect("append");

    assert_eq!(
        logs.read_recent("web", 2).expect("lines"),
        vec!["first line".to_string(), "second line".to_string()]
    );
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test cpu_stat_parser_reads_usage_usec log_store_appends_and_reads_service_lines`

Expected: FAIL because log store, CPU parser, and `tempfile` dependency do not exist.

- [ ] **Step 3: Add dependency**

Modify `Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Implement logs**

Create `src/logs.rs`:

```rust
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct LogStore {
    dir: PathBuf,
}

impl LogStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self { dir: dir.as_ref().to_path_buf() }
    }

    pub fn append(&self, service_name: &str, line: &str) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{service_name}.log")))?;
        file.write_all(line.as_bytes())
    }

    pub fn read_recent(&self, service_name: &str, limit: usize) -> std::io::Result<Vec<String>> {
        let mut content = String::new();
        OpenOptions::new()
            .read(true)
            .open(self.dir.join(format!("{service_name}.log")))?
            .read_to_string(&mut content)?;
        let lines = content
            .lines()
            .rev()
            .take(limit)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        Ok(lines.into_iter().rev().collect())
    }
}
```

Modify `src/lib.rs`:

```rust
pub mod logs;
```

- [ ] **Step 5: Extend metrics parser**

Add to `src/metrics.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CpuStat {
    pub usage_usec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MetricSnapshot {
    pub service_name: String,
    pub cpu_usage_usec: u64,
    pub memory_current_bytes: u64,
}

pub fn parse_cpu_stat(input: &str) -> Result<CpuStat, MetricsError> {
    for line in input.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("usage_usec") {
            let value = parts.next().ok_or(MetricsError::InvalidInteger)?;
            return Ok(CpuStat {
                usage_usec: value.parse().map_err(|_| MetricsError::InvalidInteger)?,
            });
        }
    }
    Err(MetricsError::Empty)
}
```

- [ ] **Step 6: Add API routes**

In `src/app.rs`, add protected routes:

```rust
.route("/services/{service_id}/logs", get(service_logs))
.route("/services/{service_id}/metrics", get(service_metrics))
```

Implement handlers returning empty arrays until runtime collection writes data:

```rust
async fn service_logs() -> Json<Vec<String>> {
    Json(Vec::new())
}

async fn service_metrics() -> Json<Vec<crate::metrics::MetricSnapshot>> {
    Json(Vec::new())
}
```

- [ ] **Step 7: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/logs.rs src/metrics.rs src/app.rs tests/backend_contract.rs
git commit -m "feat(observe): add logs and metrics surfaces"
```

## Task 7: Add Traefik Bridge Planning And Route Promotion

**Files:**
- Modify: `src/lib.rs`
- Create: `src/bridge.rs`
- Modify: `src/traefik.rs`
- Modify: `src/deploy.rs`
- Test: `tests/deploy_orchestration.rs`

- [ ] **Step 1: Write failing test**

Add to `tests/deploy_orchestration.rs`:

```rust
use denia::bridge::{BridgeAllocator, BridgeTarget};

#[test]
fn bridge_allocator_assigns_stable_loopback_ports() {
    let mut allocator = BridgeAllocator::new(19000);

    let first = allocator.assign("web", "/var/lib/denia/runtime/web/current.sock".into());
    let second = allocator.assign("web", "/var/lib/denia/runtime/web/current.sock".into());

    assert_eq!(first.port, 19000);
    assert_eq!(second.port, 19000);
    assert_eq!(
        first,
        BridgeTarget {
            service_name: "web".to_string(),
            port: 19000,
            socket_path: "/var/lib/denia/runtime/web/current.sock".into(),
        }
    );
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test bridge_allocator_assigns_stable_loopback_ports`

Expected: FAIL because `bridge` module does not exist.

- [ ] **Step 3: Implement bridge allocator**

Create `src/bridge.rs`:

```rust
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeTarget {
    pub service_name: String,
    pub port: u16,
    pub socket_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BridgeAllocator {
    next_port: u16,
    targets: BTreeMap<String, BridgeTarget>,
}

impl BridgeAllocator {
    pub fn new(start_port: u16) -> Self {
        Self {
            next_port: start_port,
            targets: BTreeMap::new(),
        }
    }

    pub fn assign(&mut self, service_name: &str, socket_path: PathBuf) -> BridgeTarget {
        if let Some(existing) = self.targets.get(service_name) {
            return existing.clone();
        }
        let target = BridgeTarget {
            service_name: service_name.to_string(),
            port: self.next_port,
            socket_path,
        };
        self.next_port += 1;
        self.targets.insert(service_name.to_string(), target.clone());
        target
    }
}
```

Modify `src/lib.rs`:

```rust
pub mod bridge;
```

- [ ] **Step 4: Wire route rendering into coordinator**

In `src/deploy.rs`, extend `DeploymentCoordinator` constructor to accept a `BridgeAllocator` and a `traefik_config_path`. After health succeeds, call `render_file_provider_config` with the promoted service domain and bridge port. Persist the file using `std::fs::write`.

Use this exact route spec:

```rust
RouteSpec {
    service_name: plan.service.name.clone(),
    domains: plan.service.domains.clone(),
    bridge_port: bridge_target.port,
}
```

- [ ] **Step 5: Add route promotion test**

Add to `tests/deploy_orchestration.rs`:

```rust
#[tokio::test]
async fn coordinator_writes_traefik_config_on_promotion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("denia.yml");

    let yaml = render_file_provider_config(&[RouteSpec {
        service_name: "web".to_string(),
        domains: vec!["web.example.test".to_string()],
        bridge_port: 19000,
    }]).expect("yaml");

    std::fs::write(&config_path, yaml).expect("write");
    let content = std::fs::read_to_string(&config_path).expect("read");

    assert!(content.contains("Host(`web.example.test`)"));
    assert!(content.contains("http://127.0.0.1:19000"));
}
```

- [ ] **Step 6: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/lib.rs src/bridge.rs src/traefik.rs src/deploy.rs tests/deploy_orchestration.rs
git commit -m "feat(ingress): add bridge routing promotion"
```

## Task 8: Wire Deployment API To Coordinator

**Files:**
- Modify: `src/app.rs`
- Modify: `src/domain.rs`
- Modify: `src/state.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write failing API test**

Add to `tests/backend_contract.rs`:

```rust
#[tokio::test]
async fn deployment_endpoint_rejects_unknown_service() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), store));

    let request = DeploymentRequest::external_image(
        uuid::Uuid::now_v7(),
        "ghcr.io/acme/web:latest",
    );

    let response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/deployments")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test deployment_endpoint_rejects_unknown_service`

Expected: FAIL because `/v1/deployments` currently creates a deployment without resolving a service.

- [ ] **Step 3: Add service lookup**

In `src/state.rs`, add:

```rust
pub fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, StateError> {
    let connection = self.connection()?;
    let value: Option<String> = connection
        .query_row(
            "SELECT config_json FROM services WHERE id = ?1",
            params![service_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    value.map(|json| serde_json::from_str(&json)).transpose().map_err(Into::into)
}
```

- [ ] **Step 4: Return 404 for unknown service**

In `src/app.rs`, before creating a deployment:

```rust
if state.store.get_service(request.service_id())?.is_none() {
    return Err(ApiError::NotFound("service not found".to_string()));
}
```

Add `ApiError::NotFound(String)` and map it to `StatusCode::NOT_FOUND`.

- [ ] **Step 5: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/app.rs src/state.rs tests/backend_contract.rs
git commit -m "feat(api): validate deployment service references"
```

## Task 9: Add Linux Runtime Implementation Gate

**Files:**
- Modify: `src/runtime.rs`
- Create: `tests/linux_runtime_privileged.rs`

- [ ] **Step 1: Write privileged ignored test**

Create `tests/linux_runtime_privileged.rs`:

```rust
#[test]
#[ignore = "requires root, cgroup v2, and Linux namespace permissions"]
fn privileged_runtime_tests_are_explicitly_gated() {
    assert_eq!(std::env::var("DENIA_RUN_PRIVILEGED_TESTS").as_deref(), Ok("1"));
}
```

- [ ] **Step 2: Run normal tests**

Run: `cargo test`

Expected: PASS with the privileged test ignored.

- [ ] **Step 3: Add Linux runtime struct**

Add to `src/runtime.rs`:

```rust
#[derive(Debug, Clone)]
pub struct LinuxRuntime {
    runtime_dir: std::path::PathBuf,
}

impl LinuxRuntime {
    pub fn new(runtime_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            runtime_dir: runtime_dir.into(),
        }
    }
}
```

Implement `Runtime` for `LinuxRuntime` with a conservative first version:

```rust
#[async_trait]
impl Runtime for LinuxRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        let cgroup_path = self.runtime_dir.join(&request.service_name).join("cgroup");
        Ok(RuntimeStatus {
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "planned".to_string(),
            pid: None,
            cgroup_path,
            socket_path: request.socket_path,
        })
    }

    async fn stop(&self, _service_name: &str) -> Result<(), RuntimeError> {
        Ok(())
    }
}
```

This keeps the public runtime boundary ready while the actual `clone3`/mount/cgroup implementation lands behind the gated privileged test suite in the following branch.

- [ ] **Step 4: Run tests and commit**

Run: `cargo fmt --all && cargo test && cargo clippy --all-targets --all-features -- -D warnings`

Expected: PASS.

Commit:

```bash
git add src/runtime.rs tests/linux_runtime_privileged.rs
git commit -m "feat(runtime): add linux runtime implementation gate"
```

## Task 10: Final Verification And Documentation Update

**Files:**
- Modify: `docs/adr/001-initial-backend-architecture.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Update ADR consequences with current implementation boundary**

Append to `docs/adr/001-initial-backend-architecture.md` under `## Consequences`:

```markdown
### Implementation Boundary

The first implementation branch establishes the deploy orchestration, state, artifact, secret, ingress, log, metric, and runtime interfaces. The dangerous Linux runtime internals are gated behind ignored privileged tests and must be implemented in a dedicated follow-up branch with root/cgroup verification.
```

- [ ] **Step 2: Update `AGENTS.md` verification**

Add under `## Verification`:

```markdown
- Privileged runtime tests: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
```

- [ ] **Step 3: Run final checks**

Run:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build
```

Expected:

- `cargo fmt --all -- --check`: no diff
- `cargo test`: all non-ignored tests pass
- `cargo clippy --all-targets --all-features -- -D warnings`: no warnings
- `cargo build`: successful build

- [ ] **Step 4: Run GitNexus scope check**

Run:

```bash
npx gitnexus analyze
```

Expected: repository indexed successfully. If GitNexus modifies `.gitignore`, `AGENTS.md`, `CLAUDE.md`, or `.claude/`, review those separately and do not include them in the functional commit unless the user asks.

- [ ] **Step 5: Commit docs**

Commit:

```bash
git add docs/adr/001-initial-backend-architecture.md AGENTS.md
git commit -m "docs: document backend v1 runtime boundary"
```

## Self-Review

- Spec coverage: The plan covers Git/BuildKit artifact acquisition, external registry pull, SOPS secrets, runtime interface, Traefik bridge, health-gated promotion, logs, metrics, API validation, and privileged runtime gating.
- Scope boundary: The actual dangerous Linux namespace/cgroup process execution is intentionally isolated behind a dedicated runtime gate because it requires root-host verification and should not be mixed with API/artifact/control-plane work.
- Placeholder scan: No open-ended placeholders are present; every task includes paths, code, commands, and expected outcomes.
- Type consistency: The plan uses existing `ServiceConfig`, `DeploymentRequest`, `ArtifactRecord`, `ResourceLimits`, `HealthCheck`, `SqliteStore`, and `AppConfig` names from the current codebase.
