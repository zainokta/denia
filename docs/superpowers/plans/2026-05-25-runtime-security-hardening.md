# Plan: Runtime Security Hardening (TODO #12 / #13)

Status: Draft · Date: 2026-05-25 · Implements [spec](../specs/2026-05-25-runtime-security-hardening.md)

## Summary

Add a user namespace + uid/gid mapping, `no_new_privs`, and a dropped capability
bounding set to workload launch, via `unshare` flags + an injected `setpriv`
wrapper. Single node-wide uid range; chown the rootfs bundle at acquisition.
CLI-only, no syscall refactor.

## Steps

### 1. Config (`src/config.rs`)
- Add fields to `AppConfig`:
  - `userns_base: u32` (`DENIA_USERNS_BASE`, default `100000`)
  - `userns_size: u32` (`DENIA_USERNS_SIZE`, default `65536`)
  - `setpriv_binary: PathBuf` (`DENIA_SETPRIV_BINARY`, default `setpriv`) — must
    point at a **statically linked** setpriv.
- Parse in `from_env`; mirror defaults in `for_test`.

### 2. Runtime (`src/runtime.rs`)
- `LinuxRuntime` gains `userns_base: u32`, `userns_size: u32`,
  `setpriv_source: PathBuf`; defaults in `new_with_paths_and_launcher`
  (`100000`/`65536`/`"setpriv"`).
- Add builders `with_userns(base, size)` and `with_setpriv(path)`.
- `plan()`: prepend `--user --map-users=0,{base},{size}
  --map-groups=0,{base},{size}`; after the `--` insert the wrapper
  `/.denia/setpriv --no-new-privs --bounding-set -all --` before `process.argv`.
  Add a `const SETPRIV_TARGET: &str = "/.denia/setpriv";`.
- `prepare()`: new `inject_setpriv(rootfs)` — resolve the setpriv binary (bare
  name via `$PATH`, else literal path), copy to `<rootfs>/.denia/setpriv`, chmod
  `0o755`. Add `RuntimeError::SetprivUnavailable { path }`. Needs
  `use std::os::unix::fs::PermissionsExt`.

### 3. Rootfs ownership (`src/artifacts/acquirer.rs`)
- In `materialize_rootfs_bundle`, after `oci_unpack_binary unpack` and creating
  `rootfs`, run `chown -R --no-dereference {base}:{base} <rootfs>` via the
  existing `CommandRunner`, with `base` from `self.config.userns_base`.

### 4. Wire (`src/app.rs`)
- `AppState::new`: build the runtime as
  `LinuxRuntime::new(config.runtime_dir.clone())
     .with_userns(config.userns_base, config.userns_size)
     .with_setpriv(config.setpriv_binary.clone())`.

### 5. ADR + docs
- New `docs/adr/005-runtime-security-hardening.md` (Proposed): record the
  userns/uid-map/cap-drop/`no_new_privs` decision, CLI approach, single uid range
  + chown, injected static setpriv, and the #13 finding. Add a row to
  `docs/adr/README.md`.
- Note the new env vars + the static-setpriv requirement in `AGENTS.md`.

### 6. Tests
- Unit test in `src/runtime.rs`: `plan()` argv contains `--user`, the
  `--map-users/--map-groups` ranges from config, and the
  `/.denia/setpriv --no-new-privs --bounding-set -all --` wrapper before the
  workload argv.
- Privileged, gated test in `tests/linux_runtime_privileged.rs`: start a workload
  that writes `/proc/self/status`; assert `NoNewPrivs: 1` and a cleared `CapBnd`.
- Add a skipped/guarded failure assertion or preflight check for kernels without
  user-namespace support, so CI failures distinguish unsupported kernel config
  from runtime regressions.

## Files

- Edit: `src/config.rs`, `src/runtime.rs`, `src/artifacts/acquirer.rs`,
  `src/app.rs`, `docs/adr/README.md`, `AGENTS.md`.
- New: `docs/adr/005-runtime-security-hardening.md`.
- Tests: `src/runtime.rs` (unit), `tests/linux_runtime_privileged.rs` (gated).

## Verification

1. `cargo build`, `cargo fmt --all`, `cargo clippy --all-targets --all-features`.
2. `cargo test` — `plan()` unit test green; existing suite green.
3. Privileged (opt-in):
   `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
   asserts `NoNewPrivs: 1` + cleared `CapBnd`.
4. Manual: deploy an external image; `ps -o uid,pid,cmd` shows the workload
   running as `BASE`, not root.

## Out of scope

Seccomp, network namespace, per-service uid ranges, idmapped mounts; the other 7
TODO sub-projects.
