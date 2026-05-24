# Spec: Rustix Syscall Refactor — Replace CLI Isolation Tools

**Status:** Draft · **Date:** 2026-05-25 · **Resolves:** TODO #13

## Problem

Denia's `LinuxRuntime` uses CLI tools (`unshare`, `setpriv`, `chown -R`, `kill`) for namespace isolation, capability hardening, rootfs ownership, and process signalling. This couples Denia to host-installed binaries, adds fork+exec overhead per workload launch, and loses type-safe error handling that raw syscalls would provide.

The OCI in-process plan (`2026-05-25-inprocess-oci-acquisition`) will remove `skopeo` and `umoci` host dependencies. After that lands, 4 of the remaining 6 host tools are syscall-replaceable.

## Goal

Replace `unshare`, `setpriv`, `chown -R`, and `kill` CLI invocations with in-process syscalls via the `rustix` crate. Keep `buildctl` (BuildKit) and `sops` as host CLI dependencies (complex domain logic outside syscall territory).

## Non-Goals

- Replacing `buildctl` or `sops` — these remain CLI dependencies.
- Changing the rootfs bundle contract, `process.json`, or the `LinuxRuntime` public API shape.
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

1. **R1 — Namespace creation (replaces `unshare`).** Fork a child, unshare `CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC`, parent writes `uid_map`/`gid_map` to `/proc/<child>/`, child unmounts old proc, mounts new proc, `pivot_root` into rootfs, chdir to workdir, `execvp` the workload.
2. **R2 — Capability hardening (replaces `setpriv`).** Child sets `PR_SET_NO_NEW_PRIVS = 1` and drops all bounding-set capabilities via `PR_CAPBSET_DROP` before exec.
3. **R3 — Recursive chown (replaces `chown -R --no-dereference`).** Walk the rootfs tree, call `lchown` on every entry, map owner:group to the configured `userns_base`.
4. **R4 — Signal delivery (replaces `kill`).** Send `SIGKILL`/`SIGTERM` via `rustix::process::kill_process`.
5. **R5 — Config cleanup.** Remove `setpriv_source`/`setpriv_binary` config fields and their env vars (`DENIA_SETPRIV_BINARY`); keep `userns_base` and `userns_size`. Remove `SETPRIV_TARGET` constant and `inject_setpriv`. Remove `resolve_setpriv`.

### Non-Functional / Constraints

- **NF1 — Safety (fork + no tokio).** The forked child never calls async code — only syscalls and `execvp`. The parent side remains in a blocking `spawn_blocking` task and returns a handle for the child pid.
- **NF2 — Crate choice.** Use `rustix`, not `nix` or raw `libc` FFI. `rustix` provides `OwnedFd`/`BorrowedFd`, pure Rust (no C dep), covers all required syscalls.
- **NF3 — Typed errors at boundaries.** New `SyscallError` enum; maps into existing `RuntimeError` and `ArtifactAcquireError` variants. No panics for expected failures.
- **NF4 — Test continuity.** All existing tests pass after the refactor. The privileged test (`linux_runtime_privileged`) asserts `NoNewPrivs: 1` + cleared `CapBnd` via the new syscall path.

## Proposed Design

### New module: `src/syscall/`

```
src/syscall/
  mod.rs     — SyscallError enum, module re-exports
  ns.rs      — Namespace creation, uid_map handshake, pivot_root, exec
  caps.rs    — PR_SET_NO_NEW_PRIVS + PR_CAPBSET_DROP
  chown.rs   — Recursive lchown walk
  signal.rs  — kill(2) wrapper
```

`SyscallError` wraps `std::io::Error` with domain variants for namespace/chown/signal failures. Each variant maps to a matching `RuntimeError` or `ArtifactAcquireError` for callers.

### Crate: rustix

| syscall          | rustix API                                                           |
| ---------------- | -------------------------------------------------------------------- |
| `unshare(2)`       | `rustix::thread::unshare(CloneFlags)`                  |
| `clone(2)`         | `rustix::process::fork()` or `rustix::thread::clone_process` (TBD during impl) |
| `pivot_root(2)`    | `rustix::fs::pivot_root(new_root, put_old)`                            |
| `mount(2)`         | `rustix::mount::mount(src, target, fstype, flags, data)`               |
| `prctl(2)`         | `rustix::process::prctl(option, ...)`                                  |
| `fchownat(2)`      | `rustix::fs::chownat(dirfd, path, uid, gid, AtFlags::SYMLINK_NOFOLLOW)` |
| `kill(2)`          | `rustix::process::kill_process(pid, signal)`                           |
| `execve(2)`        | `std::os::unix::process::CommandExt::exec()`                           |
| `chdir(2)`         | `rustix::process::chdir(path)`                                         |

### User namespace handshake (the core design)

```
Parent (spawn_blocking):
  fork()
  └─ Child:
       unshare(CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC)
       signal parent "ready" (close write end of pipe → parent's read returns EOF)
       wait for parent "go" (read pipe → blocks until parent closes)
       
  Parent:
    read pipe → child unshared
    write /proc/<child_pid>/uid_map (0 <base> <size>)
    write /proc/<child_pid>/gid_map (0 <base> <size>)
    close pipe → child proceeds
    
  └─ Child (continues):
       set_no_new_privs()
       drop_bounding_caps()
       unmount old proc (umount2("/proc", MNT_DETACH))
       mount proc at "/proc"
       pivot_root(rootfs, rootfs/.put_old)
       chdir(workdir)
       execvp(argv[0], argv)
```

### What changes per file

| File | Change |
|------|--------|
| `Cargo.toml` | Add `rustix = "1"` |
| `src/syscall/mod.rs` | New |
| `src/syscall/ns.rs` | New — namespace creation + handshake |
| `src/syscall/caps.rs` | New — no_new_privs + cap bound drop |
| `src/syscall/chown.rs` | New — recursive lchown |
| `src/syscall/signal.rs` | New — kill wrapper |
| `src/lib.rs` | Add `pub mod syscall;` |
| `src/runtime.rs` | Remove `plan()`/`prepare()` CLI logic; `start()` uses syscall module; remove `SETPRIV_TARGET`, `inject_setpriv`, `resolve_setpriv`, `setpriv_source`; remove `unshare_binary` field; keep `userns_base`/`userns_size` |
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

- **Fork safety.** `fork()` in a multithreaded (tokio) process is always risky. The child must not touch any tokio state, allocators locked by other threads, or file descriptors with complex ownership. Mitigation: the child calls only async-signal-safe syscalls and `execvp`; `spawn_blocking` isolates the fork from the async runtime.
- **uid_map race.** Writing `uid_map` requires the child to have unshared `NEWUSER` and the parent to have write access to `/proc/<child>/`. If child exits before parent writes, we get ESRCH. Mitigation: pipe synchronization as shown above.
- **pivot_root constraints.** `pivot_root` requires `new_root` to be a mount point and `put_old` to be under `new_root`. Mitigation: the rootfs is already a directory; `mount(rootfs, rootfs, "", MS_BIND|MS_REC)` before pivot if needed.
- **`execvp` path resolution.** Unlike `unshare --` which resolves `argv[0]` via `$PATH`, `execvp` does the same — no regression.
- **rustix API completeness.** `rustix` may not expose `CAPBSET_DROP` as a named constant (it is `LinuxCapability` enum). If missing, fall back to raw `libc::PR_CAPBSET_DROP` + `libc::prctl` with an `unsafe` block documented and minimal. Any such fallback is logged as a comment referencing the rustix issue/version.

## Acceptance Criteria

- [ ] `unshare`, `setpriv`, `chown -R` no longer appear in `src/` outside comments/docs.
- [ ] `DENIA_SETPRIV_BINARY` env var removed from config.
- [ ] `RuntimeError::SetprivUnavailable` removed.
- [ ] Workload host pid runs as `userns_base`, not 0.
- [ ] `/proc/<workload>/status` shows `NoNewPrivs: 1` and cleared `CapBnd`.
- [ ] `cargo build`, `cargo test`, `cargo fmt --all`, `cargo clippy --all-targets --all-features` pass.
- [ ] `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` passes.
- [ ] Existing deploy orchestration tests pass without `chown`/`kill` CLI expectations.
- [ ] ADR created (`docs/adr/006-rustix-syscall-isolation.md`).

## Open Questions

- Does `rustix` expose `CAPBSET_DROP` directly or does it need a `libc` fallback? (Resolve during implementation.)
- `clone_process` for the fork with CLONE_NEWPID — can rustix's fork+unshare approach work, or does NEWPID require `clone` semantics? The `unshare(2)` man page says NEWPID can be set via `unshare()` but the child becomes pid 1 only after fork. The current `unshare --fork --pid` handles this. With rustix: fork first, then unshare with NEWPID in the child (pid namespace takes effect on the next child, so the fork itself becomes pid 1). This matches `unshare(1)` behavior.
