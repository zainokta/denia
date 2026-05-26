# Plan: Runtime Security Hardening (TODO #12 / #13)

Status: Done · Date: 2026-05-25 · Implements [spec](../specs/2026-05-25-runtime-security-hardening.md)

> **Implementation note:** The implementation diverged from the original CLI/setpriv
> approach and instead uses direct `rustix` syscalls through `src/syscall/`. See
> ADR-005 for rationale. The plan below reflects the actual shipped implementation.

## Summary

Add a user namespace + uid/gid mapping, `no_new_privs`, and a dropped capability
bounding set to workload launch, via Denia's native fork/unshare/exec syscall
adapter (`src/syscall/ns.rs`). Single node-wide uid range; chown the rootfs
bundle at acquisition. `no_new_privs` and capability bounding-set drop are
applied in-process via `rustix` syscalls (`src/syscall/caps.rs`).

## Steps

### 1. Config (`src/config.rs`) — DONE
- Add fields to `AppConfig`:
  - `userns_base: u32` (`DENIA_USERNS_BASE`, default `100000`)
  - `userns_size: u32` (`DENIA_USERNS_SIZE`, default `65536`)
- Parse in `from_env`; mirror defaults in `for_test`.

### 2. Syscall adapter (`src/syscall/`) — DONE
- `caps.rs`: `set_no_new_privs()`, `try_set_no_new_privs()`,
  `drop_bounding_caps()`, `try_drop_bounding_caps()` via `rustix::thread`.
- `ns.rs`: `NamespaceConfig` with `userns`, `uid_map`, `gid_map`,
  `no_new_privs`, `drop_bounding_caps` fields. `spawn_namespaced_process()`
  handles fork, unshare, uid/gid map writing, cgroup attach, mount proc,
  `no_new_privs` + capability bounding-set drop, and exec.
- `chown.rs`: `recursive_lchown()` for rootfs ownership via `rustix::fs`.

### 3. Runtime (`src/runtime.rs`) — DONE
- `LinuxRuntime` gains `userns_base: u32`, `userns_size: u32`; defaults
  `100000`/`65536` in `new_with_paths`.
- Builder `with_userns(base, size)`.
- `plan()`: builds a `NamespaceConfig` with uid/gid maps from config, cgroup
  path, env, and argv. Services use `with_deferred_hardening()` (proxy brings up
  loopback first). Jobs use default immediate hardening.
- `prepare()`: injects socket-proxy binary into rootfs `/.denia/`, sets up
  socket directory with chown'd permissions.
- `chown_socket_directory()` uses `recursive_lchown` for socket dir ownership.

### 4. Rootfs ownership (`src/artifacts/acquirer.rs`) — DONE
- In `materialize_rootfs_bundle_inprocess`, after OCI unpack, runs
  `syscall::chown::recursive_lchown` on the rootfs directory with
  `self.config.userns_base`. Gracefully ignores "Operation not permitted" for
  unprivileged development.

### 5. Wire (`src/app.rs`) — DONE
- `AppState::new`: build the runtime as
  `LinuxRuntime::new_with_paths(...)
     .with_userns(config.userns_base, config.userns_size)
     .with_socket_proxy(config.socket_proxy_binary.clone())
     .with_log_dir(config.log_dir.clone())`.

### 6. ADR + docs — DONE
- `docs/adr/005-runtime-security-hardening.md`: records the syscall-based
  approach, single uid range + chown, and the decision to drop setpriv in favor
  of in-process rustix syscalls.
- `AGENTS.md`: notes `DENIA_USERNS_BASE`/`DENIA_USERNS_SIZE` env vars and that
  `setpriv` host binary is no longer required.

### 7. Tests — DONE
- Unit test in `src/runtime.rs`: `plan_includes_user_namespace_and_socket_proxy_stage`
  verifies `uid_map`/`gid_map` ranges from config and socket-proxy argv wrapping.
- Privileged, gated test in `tests/linux_runtime_privileged.rs`:
  `hardened_workload_has_no_new_privs_and_cleared_cap_bnd` starts a workload
  that writes `/proc/self/status`; asserts `NoNewPrivs: 1` and cleared `CapBnd`.

## Files

- Edit: `src/config.rs`, `src/runtime.rs`, `src/syscall/caps.rs`,
  `src/syscall/ns.rs`, `src/syscall/chown.rs`, `src/artifacts/acquirer.rs`,
  `src/app.rs`, `docs/adr/README.md`, `AGENTS.md`.
- New: `src/syscall/mod.rs`, `src/syscall/signal.rs`,
  `docs/adr/005-runtime-security-hardening.md`.
- Tests: `src/runtime.rs` (unit), `tests/linux_runtime_privileged.rs` (gated).

## Verification

1. `cargo build`, `cargo fmt --all`, `cargo clippy --all-targets --all-features`.
2. `cargo test` — unit tests green; existing suite green (143 passed, 3 ignored).
3. Privileged (opt-in):
   `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
   asserts `NoNewPrivs: 1` + cleared `CapBnd`.
4. Manual: deploy an external image; `ps -o uid,pid,cmd` shows the workload
   running as `BASE`, not root.

## Out of scope

Seccomp, network namespace, per-service uid ranges, idmapped mounts; the other 7
TODO sub-projects.
