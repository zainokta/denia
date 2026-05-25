# Plan: Rustix Syscall Refactor — Replace CLI Isolation Tools

**Status:** Draft · **Date:** 2026-05-25 · **Implements:** [spec](../specs/2026-05-25-rustix-syscall-refactor.md)


**Goal:** Replace `unshare`, `setpriv`, `chown -R`, and test `kill` CLI invocations with in-process syscalls via `rustix` where available. Remove `setpriv` binary injection entirely.

**Pre-requisites:** Runtime security hardening should land first because this plan removes its `setpriv` and `chown` CLI pieces. The OCI in-process plan (`2026-05-25-inprocess-oci-acquisition`) should also land first because it touches the same `acquirer.rs` file and the rootfs materialization path.

---

## Steps

### 1. Crate + module scaffold

**Files:**
- Edit: `Cargo.toml` — add `rustix = { version = "1", features = ["fs", "mount", "pipe", "process", "thread"] }` to `[dependencies]`; add `libc` only if process creation / exec needs a tiny fallback not exposed by `rustix`
- Edit: `src/lib.rs` — after `pub mod secrets;` add `pub mod syscall;`
- New: `src/syscall/mod.rs`

`mod.rs` exports:

```rust
pub mod caps;
pub mod chown;
pub mod ns;
pub mod signal;

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyscallError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("namespace creation failed: {0}")]
    Namespace(String),
    #[error("capability drop failed: {0}")]
    Capability(String),
    #[error("chown failed at {path}: {reason}")]
    Chown { path: std::path::PathBuf, reason: String },
    #[error("signal delivery failed for pid {pid}: {reason}")]
    Signal { pid: u32, reason: String },
    #[error("fork failed: {0}")]
    Fork(String),
}
```

### 2. `src/syscall/caps.rs`

```rust
use crate::syscall::SyscallError;

pub fn set_no_new_privs() -> Result<(), SyscallError> {
    rustix::thread::set_no_new_privs(true)
        .map_err(|e| SyscallError::Capability(format!("PR_SET_NO_NEW_PRIVS: {e}")))
}

pub fn drop_bounding_caps() -> Result<(), SyscallError> {
    // Iterate all rustix::thread::Capability variants supported by the crate.
    // If rustix has no iterator, keep a local list of variants and update it with
    // the rustix version bump.
    for cap in all_capabilities() {
        rustix::thread::remove_capability_from_bounding_set(cap)
            .map_err(|e| SyscallError::Capability(format!("drop cap {cap:?}: {e}")))?;
    }
    Ok(())
}
```

Do not default to `libc::prctl`; rustix exposes `remove_capability_from_bounding_set`. Add a `libc` fallback only if a specific capability cannot be represented by rustix, and document that narrow gap.

### 3. `src/syscall/chown.rs`

Recursive lchown of a directory tree:

```rust
use rustix::fs::{chownat, AtFlags};
use rustix::process::{Gid, Uid};
use std::path::Path;

pub fn recursive_lchown(root: &Path, uid: u32, gid: u32) -> Result<(), SyscallError> {
    let uid = Uid::from_raw(uid);
    let gid = Gid::from_raw(gid);
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| SyscallError::Io(e.into()))?;
        chownat(
            rustix::fs::CWD,
            entry.path(),
            Some(uid),
            Some(gid),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|e| SyscallError::Io(e.into()))?;
    }
    Ok(())
}
```

Wait — I need to check if `walkdir` is already a dependency. If not, use `std::fs::read_dir` recursion to avoid a new crate. The plan should handle both.

**Decision during implementation:** Check `Cargo.toml` for `walkdir`. If absent, hand-roll recursion with `std::fs::read_dir` to keep dep footprint minimal.

### 4. `src/syscall/signal.rs`

```rust
use rustix::process::{Pid, Signal};

pub fn kill(pid: u32, signal: Signal) -> Result<(), SyscallError> {
    let Some(raw_pid) = Pid::from_raw(pid as i32) else {
        return Err(SyscallError::Signal {
            pid,
            reason: "invalid pid".to_string(),
        });
    };
    rustix::process::kill_process(raw_pid, signal).map_err(|e| SyscallError::Signal {
        pid,
        reason: e.to_string(),
    })
}
```

### 5. `src/syscall/ns.rs` — core namespace orchestration

This is the largest piece. Do not implement the naive "fork, unshare NEWPID, exec" shape: `unshare(CLONE_NEWPID)` affects the next child, not the caller. Use either `clone`/`clone3` with `CLONE_NEWPID` directly if available, or a stage-1 process that calls `unshare(CLONE_NEWPID)` and then forks the workload.

```rust
use rustix::thread::CloneFlags;
use std::os::fd::OwnedFd;
use std::path::Path;

pub struct NamespaceConfig<'a> {
    pub userns_base: u32,
    pub userns_size: u32,
    pub rootfs: &'a Path,
    pub workdir: &'a Path,
    pub argv: &'a [String],
    pub env: &'a [(String, String)],
    pub cgroup_path: &'a Path,
}

pub fn spawn_namespaced_process(config: NamespaceConfig) -> Result<u32, SyscallError> {
    // Parent side:
    // 1. Prebuild CString argv/env before fork/clone.
    // 2. Create two pipes: stage1_ready and stage1_go.
    // 3. Fork/clone a stage-1 child.
    // 4. Wait for READY after stage1 unshares/enters NEWUSER.
    // 5. Write /proc/<stage1>/setgroups = "deny", then uid_map/gid_map.
    // 6. Write stage1 pid to cgroup.procs before release so workload inherits cgroup.
    // 7. Release stage1 via stage1_go.
    // 8. Return the tracked host pid. If stage1 reports a separate workload host pid,
    //    return that pid; otherwise return stage1 pid and have stage1 forward signals.
    todo!("implementation follows the handshake above")
}

fn stage1(config: NamespaceConfig, ready: OwnedFd, go: OwnedFd) -> ! {
    // Child side:
    // 1. unshare_unsafe(NEWUSER | NEWPID | NEWNS | NEWUTS | NEWIPC), or use clone/clone3 equivalent.
    // 2. signal READY; wait for GO.
    // 3. set_no_new_privs(true); drop bounding caps.
    // 4. make mount propagation private; bind-mount rootfs onto itself.
    // 5. pivot_root via rustix::process::pivot_root; unmount /.put_old.
    // 6. create /proc if needed; mount proc inside the new root.
    // 7. chdir to workdir.
    // 8. If using unshare(CLONE_NEWPID), fork workload now so it enters the new PID namespace.
    // 9. workload child execve with prebuilt C strings; stage1 waits/reaps.
    unreachable!()
}
```

Edge cases:
- **fork() panic on EAGAIN/ENOMEM**: rustix returns `Err` — handle gracefully.
- **uid_map write failure**: usual cause is a prior write with different range. Clean up child, return error.
- **pivot_root fails**: ensure rootfs is a mount point (bind-mount it first as shown).
- **cgroup assignment race**: parent must assign stage1 to `cgroup.procs` before releasing it to fork/exec workload, otherwise workload can run outside the intended cgroup.

### 6. Wire `LinuxRuntime`

**Files:**
- Edit: `src/runtime.rs`

Changes:
- Remove: `unshare_binary`, `setpriv_source`, `SETPRIV_TARGET`
- Remove: `plan()`, `prepare()`, `inject_setpriv()`, `resolve_setpriv()`
- Remove: `LinuxRuntimePlan` struct (no longer needed — cgroup/rootfs paths are computed inline)
- Remove: `RuntimeError::SetprivUnavailable` (setpriv binary is gone)
- Keep: `RuntimeError::EmptyArgv`, `RuntimeError::InvalidArgv`, `RuntimeError::InvalidWorkdir`, `RuntimeError::InvalidEnvironmentKey` — these are still used by `validate_process_spec` called inline in `start()`
- Keep: `validate_service_name`, `safe_artifact_name`, `cpu_max`, `remove_dir_if_exists`, `validate_resource_limits`
- Rewrite `start()`:
  1. Validate service name, artifact kind, resource limits
  2. Compute rootfs path, cgroup path, deployment dir
  3. Create deployment dir, socket dir, cgroup dirs
  4. Write `cpu.max`, `memory.max`
  5. `spawn_blocking(|| spawn_namespaced_process(config)).await` → get tracked host pid; this function performs the cgroup-before-exec barrier internally
  6. Return `RuntimeStatus`

```rust
#[async_trait]
impl Runtime for LinuxRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        validate_service_name(&request.service_name)?;
        if request.artifact.kind != ArtifactKind::RootfsBundle {
            return Err(RuntimeError::UnsupportedArtifactKind { kind: request.artifact.kind.clone() });
        }
        validate_resource_limits(&request)?;

        let bundle_dir = self.artifact_dir.join(safe_artifact_name(&request.artifact.digest));
        let rootfs_path = bundle_dir.join("rootfs");
        if !rootfs_path.exists() {
            return Err(RuntimeError::MissingRootfs { path: rootfs_path });
        }
        let manifest_path = bundle_dir.join("process.json");
        let manifest = std::fs::read_to_string(&manifest_path)?;
        let process: LinuxRuntimeProcessSpec = serde_json::from_str(&manifest)?;
        validate_process_spec(&process, &manifest_path)?;

        let service_dir = self.runtime_dir.join(&request.service_name);
        let deployment_dir = service_dir.join(request.deployment_id.to_string());
        let cgroup_path = self.cgroup_root.join(&request.service_name)
            .join(request.deployment_id.to_string());

        std::fs::create_dir_all(&deployment_dir)?;
        if let Some(socket_dir) = request.socket_path.parent() {
            std::fs::create_dir_all(socket_dir)?;
        }
        std::fs::create_dir_all(&cgroup_path)?;
        std::fs::write(cgroup_path.join("cpu.max"), cpu_max(request.cpu_millis))?;
        std::fs::write(cgroup_path.join("memory.max"), format!("{}\n", request.memory_bytes))?;

        let userns_base = self.userns_base;
        let userns_size = self.userns_size;
        let rootfs = rootfs_path.clone();
        let argv = process.argv.clone();
        let env = process.env.clone();
        let cgroup_for_child = cgroup_path.clone();
        let pid = tokio::task::spawn_blocking(move || {
            let config = crate::syscall::ns::NamespaceConfig {
                userns_base,
                userns_size,
                rootfs: &rootfs,
                workdir: Path::new(&process.workdir),
                argv: &argv,
                env: &env,
                cgroup_path: &cgroup_for_child,
            };
            crate::syscall::ns::spawn_namespaced_process(config)
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .await
        .map_err(|e| RuntimeError::Io(std::io::Error::other(e.to_string())))?
        .map_err(|e| RuntimeError::Io(e))?;

        Ok(RuntimeStatus {
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid: Some(pid),
            cgroup_path: cgroup_path.to_string_lossy().into_owned(),
            socket_path: request.socket_path,
        })
    }
}
```

Remove the `children` map (parent no longer tracks `tokio::process::Child` — stop() uses the syscall signal module with the stored pid). Add a `pids: Arc<Mutex<HashMap<String, u32>>>` to track pid-to-service mapping for `stop()`. If stage1 remains as a supervisor, make signal semantics explicit: either signal the reported workload host pid or signal stage1 and have it forward/terminate the workload.

### 7. Wire `ArtifactAcquirer`

**Files:**
- Edit: `src/artifacts/acquirer.rs`

Replace `runner.run("chown", ...)` in `materialize_rootfs_bundle`:

```rust
// Before:
runner.run("chown", &["-R", "--no-dereference", &owner, &rootfs_str]).await?;

// After:
crate::syscall::chown::recursive_lchown(&rootfs, self.config.userns_base, self.config.userns_base)
    .map_err(|e| ArtifactAcquireError::Io(std::io::Error::other(e.to_string())))?;
```

Remove the `owner`/`rootfs_str` intermediate variables.

### 8. Config cleanup

**Files:**
- Edit: `src/config.rs`

Remove:
- `setpriv_binary: PathBuf` field from `AppConfig`
- `DENIA_SETPRIV_BINARY` env var parsing in `from_env`
- `setpriv_binary` default in `for_test`

Keep: `userns_base`, `userns_size`.

**Files:**
- Edit: `src/app.rs`

Remove `.with_setpriv(config.setpriv_binary.clone())` from `LinuxRuntime` construction.

### 9. Test updates

**Files:**
- Edit: `src/runtime.rs` tests

The `plan_includes_user_namespace_and_setpriv_wrapper` test is replaced by a unit test that constructs a `NamespaceConfig` from a `RuntimeStartRequest` and asserts the fields (`userns_base`, `userns_size`, argv, env, rootfs, workdir, cgroup_path) are correct. No actual fork/exec is performed in unit tests — the privileged integration test covers the full syscall path.

- Edit: `tests/linux_runtime_privileged.rs`

Replace `std::process::Command::new("kill")` with `crate::syscall::signal::kill`. Replace `Command::new("chown")` with the syscall chown. The test assertions (`NoNewPrivs: 1`, cleared `CapBnd`) remain unchanged — that's the validation that the syscall path works.

- Edit: `tests/deploy_orchestration.rs`

Replace `std::process::Command::new("kill")` with the syscall signal wrapper.

- Edit: `tests/backend_contract.rs`

If `FakeCommandRunner` expectations include `chown` — remove those expectations; the acquirer no longer shells out for chown.

### 10. ADR + docs

**Files:**
- New: `docs/adr/006-rustix-syscall-isolation.md`

Record:
- Decision to use `rustix` over `nix` (safety, no C dep, sufficient API surface)
- Decision to use `fork()` + `unshare()` rather than `clone()` for namespace creation
- Decision to keep `buildctl` + `sops` as host CLI dependencies
- The two-process handshake for user namespace uid_map
- The `setpriv` binary injection is removed entirely
- Add row to `docs/adr/README.md`

**Files:**
- Edit: `AGENTS.md`

Remove `DENIA_SETPRIV_BINARY` reference, add `rustix` to the Rust conventions section. Note that `fork()` in `spawn_blocking` is the new isolation boundary.

---

## Verification

1. `cargo build` — compiles cleanly, no `unshare`/`setpriv`/`chown`-CLI in src.
2. `cargo fmt --all`
3. `cargo clippy --all-targets --all-features` — no new warnings.
4. `cargo test` — full suite green.
5. `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` — `NoNewPrivs: 1` + cleared `CapBnd` still asserted, workload pid != 0, and `NSpid` proves the workload is inside the new PID namespace.
6. `grep -rn "chown\|setpriv\|unshare" src/ | grep -v comment` — returns nothing (outside doc comments).

## Risks / Watch-Points

- **fork() in tokio:** `spawn_blocking` on a dedicated thread is the least bad pattern, but child code must not touch tokio state, logging, or allocation-heavy std APIs after fork. Prebuild argv/env C strings before fork and call only the small syscall/exec path in the child. Set a deploy timeout so a stuck stage1 cannot hang forever.
- **walkdir dependency:** Check if `walkdir` is already in `Cargo.toml`. If not, avoid adding it and hand-roll `std::fs::read_dir` recursion in `chown.rs` (~20 lines).
- **CAPBSET_DROP via rustix:** rustix exposes `remove_capability_from_bounding_set`; use it first. Fall back to `libc::prctl(PR_CAPBSET_DROP, ...)` only for a documented rustix coverage gap.
- **uid_map ordering:** `setgroups deny` must be written before `gid_map`. The code above does this correctly.
- **pivot_root on non-mount rootfs:** The bind-mount of rootfs onto itself works because `mount(..., MS_BIND|MS_REC)` makes rootfs a mount point. Remove the bind mount from `.put_old` cleanup after pivot_root.
- **child cleanup on error:** If uid_map write fails, the child is stuck waiting for the parent pipe — parent must kill the child. Add a `Drop` or explicit `kill` on the child pid for error paths.
- **PID namespace trap:** Never exec workload directly in a process that only called `unshare(CLONE_NEWPID)`. It stays in the old PID namespace; only the next child enters the new one.
