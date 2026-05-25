# Spec: Rustix Syscall Refactor — Replace CLI Isolation Tools

**Status:** Draft · **Date:** 2026-05-25 · **Resolves:** TODO #13

## Problem

Denia's `LinuxRuntime` uses CLI tools (`unshare`, `setpriv`, `chown -R`) for namespace isolation, capability hardening, and rootfs ownership. Some tests also shell out to `kill`. This couples Denia to host-installed binaries, adds fork+exec overhead per workload launch, and loses typed syscall error handling.

This refactor assumes the runtime-security-hardening plan has landed first, because it introduces `setpriv`, userns config, and rootfs ownership changes. The OCI in-process plan (`2026-05-25-inprocess-oci-acquisition`) should also land first, because it touches the same acquisition path and removes `skopeo`/`umoci`.

## Goal

Replace `unshare`, `setpriv`, `chown -R`, and test `kill` CLI invocations with in-process syscalls via `rustix` where available, plus a tightly isolated `libc` fallback only where `rustix` lacks a wrapper. Keep `buildctl` (BuildKit) and `sops` as host CLI dependencies (complex domain logic outside syscall territory).

## Non-Goals

- Replacing `buildctl` or `sops` — these remain CLI dependencies.
- Changing the rootfs bundle contract, `process.json`, or the `Runtime` trait API. Internal `LinuxRuntime` helpers such as `plan()` may change.
- Network namespace or seccomp (out of scope for this pass).
- Removing cgroup v2 writes — those are already direct filesystem operations, not CLI calls.

## Background / Current Behavior

**`src/runtime.rs`** (`LinuxRuntime`):
- `plan()` — builds an argv for `unshare ... --user --map-users=... --fork --pid --mount --uts --ipc --mount-proc --root <rootfs> --wd <workdir> -- /.denia/setpriv --no-new-privs --bounding-set -all -- <workload>`.
- `prepare()` — injects a static `setpriv` binary into `<rootfs>/.denia/setpriv`; creates cgroup dirs and writes `cpu.max`/`memory.max`.
- `inject_setpriv()` — resolves the setpriv binary via `$PATH` or literal path, copies it into the rootfs, chmod 0755.
- `start()` — spawns the assembled command via `tokio::process::Command`, moves the child pid into cgroup.

**`src/artifacts/acquirer.rs`** (`materialize_rootfs_bundle`):
- `runner.run("chown", &["-R", "--no-dereference", &owner, &rootfs_str])` — recursive chown via CLI.

**`tests/deploy_orchestration.rs`** / **`tests/linux_runtime_privileged.rs`**:
- `std::process::Command::new("kill")` — send signals to child processes.

**Cgroup writes** (`cpu.max`, `memory.max`, `cgroup.procs`):
- Already direct `std::fs::write` — no change needed.

## Requirements

### Functional

1. **R1 — Namespace creation (replaces `unshare`).** Create a paused stage-1 process in a new user/mount/UTS/IPC/PID setup, parent writes `uid_map`/`gid_map` to `/proc/<stage1>/`, parent assigns the stage-1 process to the target cgroup before release, then stage-1 prepares mounts and forks/execs the workload so the workload enters the new PID namespace. `CLONE_NEWPID` via `unshare(2)` affects the next child, not the caller; do not exec the workload directly in the process that called `unshare(CLONE_NEWPID)`.
2. **R2 — Capability hardening (replaces `setpriv`).** Child calls `rustix::thread::set_no_new_privs(true)` and drops all bounding-set capabilities via `rustix::thread::remove_capability_from_bounding_set` before exec.
3. **R3 — Recursive chown (replaces `chown -R --no-dereference`).** Walk the rootfs tree, call `lchown` on every entry, map owner:group to the configured `userns_base`.
4. **R4 — Signal delivery (replaces `kill`).** Send `SIGKILL`/`SIGTERM` via `rustix::process::kill_process`.
5. **R5 — Config cleanup.** Remove `setpriv_source`/`setpriv_binary` config fields and their env vars (`DENIA_SETPRIV_BINARY`); keep `userns_base` and `userns_size`. Remove `SETPRIV_TARGET` constant and `inject_setpriv`. Remove `resolve_setpriv`.

### Non-Functional / Constraints

- **NF1 — Safety (fork + no tokio).** Post-fork child code never calls async code, tokio, logging, or allocation-heavy std APIs. It uses the smallest syscall path possible and `execve` with prebuilt C strings. The parent side remains in a blocking `spawn_blocking` task and returns a handle for the tracked pid.
- **NF2 — Crate choice.** Use `rustix` for syscalls it exposes. Do not add `nix`. If `fork`/`clone`/`execve` require `libc`, isolate the unsafe calls in `src/syscall/ns.rs`, prebuild argv/env `CString`s before fork, and document why `rustix` could not cover that edge. Do not call `std::process::Command` in a post-fork child.
- **NF3 — Typed errors at boundaries.** New `SyscallError` enum; maps into existing `RuntimeError` and `ArtifactAcquireError` variants. No panics for expected failures.
- **NF4 — Test continuity.** All existing tests pass after the refactor. The privileged test (`linux_runtime_privileged`) asserts `NoNewPrivs: 1` + cleared `CapBnd` via the new syscall path.

## Proposed Design

### New module: `src/syscall/`

```
src/syscall/
  mod.rs     — SyscallError enum, module re-exports
  ns.rs      — Namespace creation, uid_map handshake, pivot_root, exec
  caps.rs    — no_new_privs + bounding capability drop
  chown.rs   — Recursive lchown walk
  signal.rs  — kill(2) wrapper
```

`SyscallError` wraps `std::io::Error` with domain variants for namespace/chown/signal failures. Each variant maps to a matching `RuntimeError` or `ArtifactAcquireError` for callers.

### Crate: rustix

| syscall          | rustix API                                                           |
| ---------------- | -------------------------------------------------------------------- |
| `unshare(2)`       | `rustix::thread::unshare_unsafe(CloneFlags)`; `unshare` is deprecated |
| `clone(2)` / `fork(2)` | Prefer `rustix` if exposed; otherwise use a minimal documented `libc` fallback in `syscall::ns` |
| `pivot_root(2)`    | `rustix::process::pivot_root(new_root, put_old)`                       |
| `mount(2)`         | `rustix::mount::mount(src, target, fstype, flags, data)`               |
| `no_new_privs`     | `rustix::thread::set_no_new_privs(true)`                               |
| cap bound drop     | `rustix::thread::remove_capability_from_bounding_set(cap)`             |
| `fchownat(2)`      | `rustix::fs::chownat(dirfd, path, uid, gid, AtFlags::SYMLINK_NOFOLLOW)` |
| `kill(2)`          | `rustix::process::kill_process(pid, signal)`                           |
| `execve(2)`        | Prefer `rustix` if exposed; otherwise minimal `libc::execve` wrapper with prebuilt `CString`s |
| `chdir(2)`         | `rustix::process::chdir(path)`                                         |

### User namespace handshake (the core design)

```
Parent (spawn_blocking):
  create two pipes: stage1_ready, stage1_go
  fork/clone stage1
  └─ Stage1:
       unshare/clone user + mount + uts + ipc + pid setup
       close stage1_ready.write or write READY
       block on stage1_go.read

  Parent:
    wait stage1_ready
    write /proc/<stage1_pid>/setgroups = "deny"
    write /proc/<stage1_pid>/uid_map = "0 <base> <size>"
    write /proc/<stage1_pid>/gid_map = "0 <base> <size>"
    write <stage1_pid> to cgroup.procs before workload fork so cgroup membership is inherited
    close/write stage1_go so stage1 proceeds

  └─ Stage1 (continues):
       set_no_new_privs(true)
       drop bounding caps
       make mount propagation private
       bind-mount rootfs onto itself
       pivot_root(rootfs, rootfs/.put_old)
       unmount /.put_old
       mount proc at /proc inside new root
       chdir(workdir)
       fork workload if using unshare(CLONE_NEWPID), because NEWPID applies to next child
       workload child execve(argv[0], argv, env)
       stage1 waits/reaps workload and exits with workload status
```

### What changes per file

| File | Change |
|------|--------|
| `Cargo.toml` | Add `rustix = { version = "1", features = ["fs", "mount", "pipe", "process", "thread"] }`; add `libc` only if needed for process creation / exec fallback |
| `src/syscall/mod.rs` | New |
| `src/syscall/ns.rs` | New — namespace creation + handshake |
| `src/syscall/caps.rs` | New — no_new_privs + cap bound drop |
| `src/syscall/chown.rs` | New — recursive lchown |
| `src/syscall/signal.rs` | New — kill wrapper |
| `src/lib.rs` | Add `pub mod syscall;` |
| `src/runtime.rs` | Remove `plan()`/`prepare()` CLI logic; `start()` uses syscall module with a cgroup-before-exec barrier; remove `SETPRIV_TARGET`, `inject_setpriv`, `resolve_setpriv`, `setpriv_source`; remove `unshare_binary` field; keep `userns_base`/`userns_size` |
| `src/artifacts/acquirer.rs` | Replace `runner.run("chown", ...)` with `syscall::chown::recursive_lchown` |
| `src/config.rs` | Remove `setpriv_binary` field + `DENIA_SETPRIV_BINARY` env var |
| `src/app.rs` | Drop `.with_setpriv(...)` builder call |
| `src/command.rs` | Unchanged (still needed for `buildctl` and `sops`) |
| `tests/linux_runtime_privileged.rs` | Switch `kill` to syscall wrapper; validate `NoNewPrivs`/`CapBnd` still present |
| `tests/deploy_orchestration.rs` | Switch `kill` to syscall wrapper |

### What is removed

- `unshare_binary` field from `LinuxRuntime`
- `setpriv_source` field from `LinuxRuntime`
- `setpriv_binary` field from `AppConfig`
- `SETPRIV_TARGET` constant
- `inject_setpriv()` method
- `resolve_setpriv()` function
- `DENIA_SETPRIV_BINARY` env var config
- `RuntimeError::SetprivUnavailable` variant
- `.with_setpriv()` builder on `LinuxRuntime`
- `plan()` method on `LinuxRuntime` (replaced by syscall module orchestration)
- `prepare()` method — cgroup writes move inline to `start()`, chown moves to acquirer

### What stays

- `userns_base` / `userns_size` config + env vars
- `CommandRunner` trait (for `buildctl` and `sops`)
- `FakeCommandRunner` (for tests exercising Git builds)
- Cgroup v2 writes (`cpu.max`, `memory.max`, `cgroup.procs`) — already syscall-style
- `UnixSocket` ingress model — unchanged
- `Runtime` trait — unchanged, `start()`/`stop()` signatures identical

## Risks

- **Fork safety.** `fork()` in a multithreaded (tokio) process is always risky. The child must not touch tokio state, logging, allocation-heavy code, or `std::process::Command`. Mitigation: prebuild argv/env C strings before fork; child calls only the small syscall path and `execve`; `spawn_blocking` keeps the parent-side orchestration off the async worker threads.
- **uid_map race.** Writing `uid_map` requires the child to have unshared `NEWUSER` and the parent to have write access to `/proc/<child>/`. If child exits before parent writes, we get ESRCH. Mitigation: pipe synchronization as shown above.
- **pivot_root constraints.** `pivot_root` requires `new_root` to be a mount point and `put_old` to be under `new_root`. Mitigation: the rootfs is already a directory; `mount(rootfs, rootfs, "", MS_BIND|MS_REC)` before pivot if needed.
- **Exec path resolution.** Denia already requires absolute `argv[0]` inside the rootfs, so the syscall path should use `execve` with that absolute path and should not rely on `$PATH` lookup.
- **rustix API completeness.** `rustix` exposes `set_no_new_privs(true)` and `remove_capability_from_bounding_set`, but may not expose every process-creation / exec primitive needed for the stage-1 launcher. Any `libc` fallback must be tiny, isolated, documented, and covered by privileged tests.

## Acceptance Criteria

- [ ] `unshare`, `setpriv`, `chown -R` no longer appear in `src/` outside comments/docs.
- [ ] `DENIA_SETPRIV_BINARY` env var removed from config.
- [ ] `RuntimeError::SetprivUnavailable` removed.
- [ ] Workload host pid runs as `userns_base`, not 0.
- [ ] Workload is actually inside a new PID namespace; privileged test checks `/proc/<pid>/status` `NSpid` includes namespace pid `1` (or equivalent proof).
- [ ] `/proc/<workload>/status` shows `NoNewPrivs: 1` and cleared `CapBnd`.
- [ ] `cargo build`, `cargo test`, `cargo fmt --all`, `cargo clippy --all-targets --all-features` pass.
- [ ] `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` passes.
- [ ] Existing deploy orchestration tests pass without `chown`/`kill` CLI expectations.
- [ ] ADR created (`docs/adr/006-rustix-syscall-isolation.md`).

## Open Questions

- Does `rustix` expose enough process-creation / exec API for the stage-1 launcher, or is a minimal `libc` fallback required?
- Should implementation use `clone`/`clone3` with `CLONE_NEWPID` directly, or `fork` + `unshare(CLONE_NEWPID)` + a stage-1 workload fork? The second shape is valid only if the stage-1 forks the workload after unshare; the unshare caller itself does not enter the new PID namespace.
