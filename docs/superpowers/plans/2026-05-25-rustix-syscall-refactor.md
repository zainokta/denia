# Plan: Rustix Syscall Refactor — Replace CLI Isolation Tools

**Status:** Draft · **Date:** 2026-05-25 · **Implements:** [spec](../specs/2026-05-25-rustix-syscall-refactor.md)


**Goal:** Replace `unshare`, `setpriv`, `chown -R`, and `kill` CLI invocations with in-process `rustix` syscalls. Remove `setpriv` binary injection entirely.

**Pre-requisite:** The OCI in-process plan (`2026-05-25-inprocess-oci-acquisition`) should land first since it touches the same `acquirer.rs` file and the chown path.

---

## Steps

### 1. Crate + module scaffold

**Files:**
- Edit: `Cargo.toml` — add `rustix = "1"` to `[dependencies]`
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
use rustix::process::PrctlFlags;
use crate::syscall::SyscallError;

pub fn set_no_new_privs() -> Result<(), SyscallError> {
    rustix::process::prctl_1arg(PrctlFlags::SET_NO_NEW_PRIVS, 1)
        .map_err(|e| SyscallError::Capability(format!("PR_SET_NO_NEW_PRIVS: {e}")))
}

pub fn drop_bounding_caps() -> Result<(), SyscallError> {
    // PR_CAPBSET_DROP for all capabilities
    // Iterate known caps; rustix may expose via LinuxCapability enum or fall back to libc
    for cap in 0..41u64 { // CAP_LAST_CAP + 1
        unsafe {
            let rc = libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0);
            if rc != 0 {
                // EINVAL means the cap doesn't exist on this kernel — non-fatal
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::EINVAL) {
                    return Err(SyscallError::Capability(format!("drop cap {cap}: {e}")));
                }
            }
        }
    }
    Ok(())
}
```

Note: If rustix adds `CAPBSET_DROP` support by implementation time, switch to it. The `unsafe` block is minimal and self-contained.

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
use rustix::process::Signal;

pub fn kill(pid: u32, signal: Signal) -> Result<(), SyscallError> {
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid as i32),
        signal,
    )
    .map_err(|e| SyscallError::Signal {
        pid,
        reason: e.to_string(),
    })
}
```

### 5. `src/syscall/ns.rs` — core namespace orchestration

This is the largest piece. The `create_namespace` function:

```rust
use rustix::thread::CloneFlags;
use rustix::process::{fork, ForkResult};
use std::os::unix::process::CommandExt;
use std::path::Path;

pub struct NamespaceConfig<'a> {
    pub userns_base: u32,
    pub userns_size: u32,
    pub rootfs: &'a Path,
    pub workdir: &'a Path,
    pub argv: &'a [String],
    pub env: &'a [(String, String)],
}

pub fn create_namespace(config: NamespaceConfig) -> Result<u32, SyscallError> {
    // 1. Create sync pipe
    let (read_fd, write_fd) = rustix::pipe::pipe()
        .map_err(|e| SyscallError::Namespace(format!("pipe: {e}")))?;

    // 2. Fork (child will unshare + exec, parent writes uid_map)
    match unsafe { fork()? } {
        ForkResult::Parent { child_pid } => {
            drop(write_fd); // close write end in parent
            // Wait for child to unshare NEWUSER
            let mut buf = [0u8; 1];
            rustix::io::read(&read_fd, &mut buf)
                .map_err(|e| SyscallError::Namespace(format!("sync read: {e}")))?;
            // Write uid_map
            let uid_map = format!("0 {} {}\n", config.userns_base, config.userns_size);
            std::fs::write(format!("/proc/{}/uid_map", child_pid.as_raw()), &uid_map)
                .map_err(|e| SyscallError::Namespace(format!("write uid_map: {e}")))?;
            // Write gid_map (needs setgroups deny first)
            std::fs::write(format!("/proc/{}/setgroups", child_pid.as_raw()), "deny")
                .map_err(|e| SyscallError::Namespace(format!("write setgroups: {e}")))?;
            let gid_map = format!("0 {} {}\n", config.userns_base, config.userns_size);
            std::fs::write(format!("/proc/{}/gid_map", child_pid.as_raw()), &gid_map)
                .map_err(|e| SyscallError::Namespace(format!("write gid_map: {e}")))?;
            drop(read_fd); // close read end → child unblocks
            Ok(child_pid.as_raw() as u32)
        }
        ForkResult::Child => {
            drop(read_fd); // close read end in child
            // Unshare namespaces
            rustix::thread::unshare(
                CloneFlags::NEWUSER | CloneFlags::NEWPID | CloneFlags::NEWNS |
                CloneFlags::NEWUTS | CloneFlags::NEWIPC
            ).map_err(|e| SyscallError::Namespace(format!("unshare: {e}")))?;
            // Signal parent we've unshared
            drop(write_fd);
            // Wait for parent to write maps
            let mut buf = [0u8; 1];
            let _ = rustix::io::read(&read_fd, &mut buf); // blocks until parent closes
            // Now drop privileges
            super::caps::set_no_new_privs()?;
            super::caps::drop_bounding_caps()?;
            // Unmount old proc, mount new proc
            rustix::mount::unmount("/proc", rustix::mount::UnmountFlags::DETACH)
                .map_err(|e| SyscallError::Namespace(format!("umount /proc: {e}")))?;
            rustix::mount::mount("proc", "/proc", "proc", rustix::mount::MountFlags::empty(), b"")
                .map_err(|e| SyscallError::Namespace(format!("mount /proc: {e}")))?;
            // Bind-mount rootfs (required for pivot_root)
            rustix::mount::mount(
                config.rootfs,
                config.rootfs,
                "",
                rustix::mount::MountFlags::BIND | rustix::mount::MountFlags::REC,
                b"",
            ).map_err(|e| SyscallError::Namespace(format!("bind mount rootfs: {e}")))?;
            // pivot_root
            let put_old = config.rootfs.join(".put_old");
            std::fs::create_dir_all(&put_old)
                .map_err(|e| SyscallError::Namespace(format!("create put_old: {e}")))?;
            rustix::fs::pivot_root(config.rootfs, &put_old)
                .map_err(|e| SyscallError::Namespace(format!("pivot_root: {e}")))?;
            // chdir to workdir (now relative to new root)
            rustix::process::chdir(config.workdir)
                .map_err(|e| SyscallError::Namespace(format!("chdir: {e}")))?;
            // execvp
            let mut cmd = std::process::Command::new(&config.argv[0]);
            cmd.args(&config.argv[1..])
                .envs(config.env.iter().map(|(k, v)| (k, v)));
            let err = cmd.exec();
            // execvp only returns on error
            Err(SyscallError::Namespace(format!("execvp: {err}")))
        }
    }
}
```

Edge cases:
- **fork() panic on EAGAIN/ENOMEM**: rustix returns `Err` — handle gracefully.
- **uid_map write failure**: usual cause is a prior write with different range. Clean up child, return error.
- **pivot_root fails**: ensure rootfs is a mount point (bind-mount it first as shown).

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
  5. `spawn_blocking(|| create_namespace(config)).await` → get child pid
  6. Write pid to `cgroup.procs`
  7. Return `RuntimeStatus`

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
        let pid = tokio::task::spawn_blocking(move || {
            let config = crate::syscall::ns::NamespaceConfig {
                userns_base,
                userns_size,
                rootfs: &rootfs,
                workdir: Path::new(&process.workdir),
                argv: &argv,
                env: &env,
            };
            crate::syscall::ns::create_namespace(config)
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .await
        .map_err(|e| RuntimeError::Io(std::io::Error::other(e.to_string())))?
        .map_err(|e| RuntimeError::Io(e))?;

        std::fs::write(cgroup_path.join("cgroup.procs"), format!("{pid}\n"))?;

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

Remove the `children` map (parent no longer tracks `tokio::process::Child` — stop() uses the syscall signal module with the stored pid). Add a `pids: Arc<Mutex<HashMap<String, u32>>>` to track pid-to-service mapping for `stop()`.

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

The `plan_includes_user_namespace_and_setpriv_wrapper` test is replaced by a unit test that constructs a `NamespaceConfig` from a `RuntimeStartRequest` and asserts the fields (`userns_base`, `userns_size`, argv, env, rootfs, workdir) are correct. No actual fork/exec is performed in unit tests — the privileged integration test covers the full syscall path.

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
5. `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` — `NoNewPrivs: 1` + cleared `CapBnd` still asserted, workload pid != 0.
6. `grep -rn "chown\|setpriv\|unshare" src/ | grep -v comment` — returns nothing (outside doc comments).

## Risks / Watch-Points

- **fork() in tokio:** `spawn_blocking` on a dedicated thread is the standard pattern. The child side never touches tokio state. If the fork itself hangs (due to another thread holding a lock), the `spawn_blocking` task will hang — set a reasonable timeout on the deploy path.
- **walkdir dependency:** Check if `walkdir` is already in `Cargo.toml`. If not, avoid adding it and hand-roll `std::fs::read_dir` recursion in `chown.rs` (~20 lines).
- **CAPBSET_DROP via rustix:** If rustix 1.x doesn't expose `PR_CAPBSET_DROP`, the minimal `unsafe { libc::prctl(...) }` block in `caps.rs` is acceptable and well-documented.
- **uid_map ordering:** `setgroups deny` must be written before `gid_map`. The code above does this correctly.
- **pivot_root on non-mount rootfs:** The bind-mount of rootfs onto itself works because `mount(..., MS_BIND|MS_REC)` makes rootfs a mount point. Remove the bind mount from `.put_old` cleanup after pivot_root.
- **child cleanup on error:** If uid_map write fails, the child is stuck waiting for the parent pipe — parent must kill the child. Add a `Drop` or explicit `kill` on the child pid for error paths.
