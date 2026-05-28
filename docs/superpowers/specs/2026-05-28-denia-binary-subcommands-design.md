# Denia Binary Subcommands: Move Host Provisioning Out Of `install.sh`

- Status: Draft
- Date: 2026-05-28
- Owner: zainokta
- Related: ADR-023 (TOML config file), `install.sh` (current installer)

## Motivation

`install.sh` currently does ten distinct things in bash: preflight, package
detection, package install, rustup, node/pnpm, web build, cargo build, system
user + on-disk layout, secret generation (age key + admin token), TOML config
emission, systemd unit emission, and service enable. It is ~900 lines and the
provisioning logic (steps 8-11) duplicates knowledge that already lives in
`src/config.rs` (paths + TOML fields) and would be naturally expressed as Rust.

The duplication causes drift: a new tunable added to `FileConfig` is silently
absent from `install.sh::write_config_file` until someone notices in
production. The `denia.service` template lives only in bash and cannot be
re-rendered after install. Operators have no way to ask the daemon "are my
paths sane?" without reading bash + Rust side-by-side.

This spec splits the installer into two surfaces:

- **`install.sh`** — OS package management + Rust toolchain + binary build,
  ending in a single binary at `/usr/local/bin/denia`. Reduced to ~150 lines.
- **`denia <subcommand>`** — everything denia-owned: provisioning, status,
  diagnosis, secret rotation, teardown. Implemented in Rust, sharing types
  and paths with the daemon itself.

The daemon's own run path (`denia` with no subcommand) is unchanged.

## Goals

- Single source of truth for the systemd unit and TOML config layout: Rust.
- Operator gets a discoverable CLI: `denia --help`, `denia setup --help`.
- Provisioning is idempotent; re-run on an existing install never destroys
  keys or data, only refreshes generated artefacts (the systemd unit).
- Drift detection: `denia doctor` notices a hand-edited systemd unit or
  missing dependency.
- Token rotation does not require a bespoke shell pipeline.

## Non-Goals

- Multi-node orchestration. Single-node, single-binary remains the model.
- Rootless / unprivileged operation. The daemon still runs as the `denia`
  system user with the same capability set (see ADR-021 and ADR-023).
- Replacing `install.sh` entirely. Package management belongs in bash; it is
  unavoidably distro-specific and runs before any Rust binary exists.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ install.sh  (bash, ~150 lines, distro-aware)                 │
│   1. preflight: OS / arch / cgroup v2 / userns / port :80 :443 │
│   2. detect_pkg_mgr (apt / dnf / pacman / zypper)            │
│   3. install build + runtime deps                            │
│      (build-essential make pkg-config libssl-dev curl git    │
│       age sops iproute2 procps + buildctl)                   │
│   4. rustup-init  →  isolated CARGO_HOME / RUSTUP_HOME       │
│   5. node 22 + pnpm via corepack                             │
│   6. make install        ← builds web + rust + copies binary │
│   echo "Now run: sudo denia setup"                           │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ /usr/local/bin/denia  (Rust, clap-driven subcommands)        │
│                                                              │
│  multi-call (existing, unchanged):                           │
│    argv[0] == "socket-proxy"       → socket_proxy::run       │
│    argv[0] == "workload-launcher"  → workload_launcher::run  │
│                                                              │
│  subcommands (new):                                          │
│    sudo denia setup            → provision + start (root)    │
│    sudo denia uninstall        → tear down service (root)    │
│         denia status           → live state (any user)       │
│         denia doctor           → diagnose host (any user)    │
│    sudo denia rotate-token     → regen + restart (root)      │
│         denia version | --version                            │
│         denia help | --help                                  │
│                                                              │
│  default (no subcommand):                                    │
│    daemon::run  ← current main.rs body, extracted as-is      │
│                  (started by systemd as User=denia + caps;   │
│                   not meant to be run by hand)               │
└──────────────────────────────────────────────────────────────┘
```

### Build Orchestration (`Makefile`)

`install.sh` no longer inlines `cd web && pnpm install && pnpm build && cargo
build --release --locked`. That sequence moves into a root `Makefile` so
contributors, CI, and the installer all share one build path. `make` is on
every Linux distro out of the box; the only new tooling cost is the `make`
package itself (already a dependency of `build-essential` / `base-devel` /
equivalents).

Targets:

| Target | Steps | Notes |
|--------|-------|-------|
| `make` (default) | `make build` | One-keystroke full build. |
| `make build` | `make web` + `make rust` | Builds in the order the binary expects (SPA must exist before `cargo build --release` for `rust-embed`). |
| `make web` | `cd web && pnpm install --frozen-lockfile && pnpm build` | Produces `web/dist/client`. |
| `make rust` | `cargo build --release --locked` | Embeds the SPA via `rust-embed`. |
| `make install` | `make build` + `install -Dm0755 target/release/denia /usr/local/bin/denia` | Used by `install.sh`. Requires root for the copy. |
| `make clean` | `cargo clean` + `rm -rf web/dist web/node_modules` | |
| `make uninstall` | `rm -f /usr/local/bin/denia` | Convenience for dev rollback. Does not touch state. |

Convention:
- `.PHONY:` declared for every target (no real files).
- `SHELL := /usr/bin/env bash` for portability across distros' default
  shells.
- Targets call the existing tools (`pnpm`, `cargo`); no recipes hide
  business logic in the Makefile itself.

## Module Structure

```
src/
  main.rs                 # multi-call → cli::dispatch → daemon::run
  daemon.rs               # extracted from current main.rs body (~200 lines)
  cli/
    mod.rs                # Cli + Commands enum, dispatch()
    setup.rs              # SetupArgs, run_setup()
    uninstall.rs          # UninstallArgs { purge: bool }, run_uninstall()
    status.rs             # run_status()
    doctor.rs             # run_doctor()
    rotate_token.rs       # run_rotate_token()
    common/
      privilege.rs        # require_root(), detect_install_user() → InstallContext
      paths.rs            # InstallPaths { binary, unit, data_dir, user_config_dir, … }
      systemd.rs          # write_unit(), daemon_reload(), enable_now(), restart(), is_active()
      provision.rs        # create_user_group(), create_data_dirs(), delegate_cgroup_root()
      secrets.rs          # generate_age_identity(), generate_admin_token()
      config_writer.rs    # render_config_toml(&InstallContext) -> String
  templates/
    denia.service.in      # systemd unit template, embedded via include_str!
    config.toml.in        # TOML template, embedded via include_str!
```

Each `cli/common/*` helper is a pure function or a small wrapper around a
single side effect, unit-testable in isolation. The shared `InstallContext`
struct carries `(install_user, install_home, user_config_dir, config_file,
token_file, age_key_file)` and is computed once at the top of every
subcommand that needs it.

### `main.rs` After The Refactor

```rust
fn main() -> anyhow::Result<()> {
    // 1) Multi-call: socket-proxy / workload-launcher (existing).
    if multicall_dispatch()? { return Ok(()); }

    // 2) Subcommand parsing.
    use cli::{Cli, Commands};
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Setup(args))     => cli::setup::run(args),
        Some(Commands::Uninstall(args)) => cli::uninstall::run(args),
        Some(Commands::Status)          => cli::status::run(),
        Some(Commands::Doctor)          => cli::doctor::run(),
        Some(Commands::RotateToken)     => cli::rotate_token::run(),
        None                            => daemon::run(),
    }
}
```

## Dependencies

Add to `Cargo.toml`:

- `clap = { version = "4", features = ["derive"] }` — CLI parsing + auto-help.
- `age = "0.10"` — generate X25519 age identity in-process; replaces
  shelling out to `age-keygen` from bash.

Both are stable, widely used. Compile-time cost: ~10s additional cold-build;
~200KB binary size impact. Acceptable for the move-host-binary-into-Rust win.

## Subcommand Behavior

### `denia setup`

**Preconditions:** `EUID == 0` and `$SUDO_USER` set + not `root`. Fail fast
with re-run instruction (`Re-run as: sudo denia setup`).

**Steps (each idempotent: probe, skip-if-present, log):**

1. `groupadd --system denia` (skip if exists).
2. `useradd --system --gid denia --home-dir /var/lib/denia --no-create-home --shell /usr/sbin/nologin denia`.
3. `mkdir -p /var/lib/denia/{sqlite,artifacts,tls,runtime,logs}` mode `0700 denia:denia`.
4. `mkdir -p /sys/fs/cgroup/denia` mode `0755 denia:denia`.
5. `mkdir -p ~/.config/denia` mode `0750 <user>:denia`.
6. Generate age identity if `~/.config/denia/age.key` absent. Use the `age`
   crate to mint an X25519 identity; format file as
   `# public key: <recipient>\n<AGE-SECRET-KEY-1...>\n`; mode
   `0640 <user>:denia`.
7. Generate admin token if `~/.config/denia/admin.token` absent. 32 random
   bytes from `OsRng` → hex; write `DENIA_ADMIN_TOKEN=<hex>\n`; mode
   `0640 <user>:denia`.
8. Write `~/.config/denia/config.toml` if absent, from `config.toml.in`
   template + InstallContext substitutions; mode `0640 <user>:denia`.
9. **Always overwrite** `/etc/systemd/system/denia.service` from
   `denia.service.in` template. The unit content must track the binary
   version; an out-of-date unit is a footgun and explicit refresh on
   `setup` keeps drift from silently accumulating.
10. `systemctl daemon-reload`.
11. `systemctl enable --now denia.service`.
12. Wait up to 30s for `systemctl is-active denia.service`; on timeout dump
    the last 50 lines of `journalctl -u denia.service` and exit non-zero.

**Summary print** (final stdout block): file table (paths, perms, owners) +
bootstrap curl one-liner referencing `~/.config/denia/admin.token`. Same
shape as today's `install.sh::step_summary`.

**Flags:**
- `--dry-run` — print the plan, do not execute. Honors per-step probes so
  the output reflects which steps would actually run.

### `denia uninstall [--purge]`

**Preconditions:** `EUID == 0`. `--purge` additionally requires `$SUDO_USER`
to resolve the operator's config directory.

**Steps:**

1. `systemctl disable --now denia.service` (ignore "not loaded").
2. `rm -f /etc/systemd/system/denia.service`.
3. `systemctl daemon-reload`.
4. **Do not** remove `/usr/local/bin/denia`. The running process *is* that
   binary; self-deletion is fragile. Print: "Remove the binary manually
   with: sudo rm /usr/local/bin/denia".
5. With `--purge`: `rm -rf /var/lib/denia ~/.config/denia`; `userdel denia`;
   `groupdel denia`; `rmdir /sys/fs/cgroup/denia 2>/dev/null || true`.

**Flags:**
- `--purge` — wipe data + user config + system user.
- `--dry-run` — print the plan only.

### `denia status`

**Preconditions:** none. Runs as any user.

**Output (plain text):**

```
denia 0.1.0+<git_sha>
service:    active (running)
config:     /home/rakei/.config/denia/config.toml
bind_addr:  127.0.0.1:7180
healthz:    200 OK
```

Resolves config path via `DENIA_CONFIG_FILE` env if set, else the standard
`FileConfig` resolution chain (ADR-023). Reads `bind_addr` from the resolved
config (no env override needed — the daemon's actual unit pins both).

Probes `/healthz` with a 2-second timeout. Exit `0` if active + healthy,
`1` otherwise.

### `denia doctor`

**Preconditions:** none. Root-only checks emit `[SKIP] needs root` when run
unprivileged.

**Checks (each PASS / FAIL / SKIP, terminal-colored):**

| Check | Source |
|-------|--------|
| cgroup v2 unified mount at `/sys/fs/cgroup` | `/proc/mounts` |
| `kernel.max_user_namespaces` > 0 | `/proc/sys/user/max_user_namespaces` |
| `:80` / `:443` free or owned by `denia.service` | `ss -ltnH` + `systemctl show -p MainPID` |
| Deps in `PATH`: `age`, `sops`, `git`, `buildctl` | `which` |
| `~/.config/denia/{config.toml,admin.token,age.key}` present + mode `0640 <user>:denia` | `stat` |
| Systemd unit content matches template hash (drift detection) | render template → SHA256 vs `/etc/systemd/system/denia.service` SHA256 |
| `/healthz` reachable on configured `bind_addr` | HTTP probe |

Exit `0` if every check is PASS or SKIP; `1` if any FAIL.

### `denia rotate-token`

**Preconditions:** `EUID == 0` and `$SUDO_USER` set.

**Steps:**

1. Generate new 32-byte hex token.
2. Atomically replace `~/.config/denia/admin.token`: write `<file>.tmp`,
   `fsync`, `rename` over the original.
3. `systemctl restart denia.service`.
4. Wait up to 15s for `is-active`.
5. Print the new token to stdout exactly once. Caller MUST capture it on
   the same run; no recovery path beyond reading `admin.token` directly.

The old token is invalidated when the daemon reloads its EnvironmentFile on
restart.

### `denia version` / `--version`

Print `denia <CARGO_PKG_VERSION>+<git_sha>` and exit `0`.

### `denia help` / `--help`

Provided by clap. `denia help <subcommand>` shows per-subcommand help.

## Error Handling

- Every subcommand returns `anyhow::Result<()>`; non-zero exit on `Err`.
- Privilege failures: explicit message + non-zero exit; do not swallow.
- Atomic file writes for secrets and config: write to `<path>.tmp` in the
  same directory, `fsync`, `rename`. No window where a partial file is
  visible to the daemon.
- Idempotent steps log `[OK] already present (kept)` vs `[OK] created`; a
  re-run is loud but harmless.
- Never log token contents, age secret material, or contents of
  `admin.token`. Log file paths and operation outcomes only.

## Testing

### Unit tests (no privilege, in `cargo test`)

- `secrets::generate_admin_token` → returns 64 hex chars, charset valid,
  two calls differ.
- `secrets::generate_age_identity` → returned string parses via the `age`
  crate; contains `# public key: age1...` comment and `AGE-SECRET-KEY-1...`
  body line.
- `config_writer::render(&InstallContext)` → output parses with
  `toml::from_str::<FileConfig>` and every path field equals the
  InstallContext field (golden test).
- `systemd::render_unit(&InstallContext)` → output contains expected
  `User=`, `Environment=DENIA_CONFIG_FILE=`, `BindReadOnlyPaths=`,
  `AmbientCapabilities=` lines; stable SHA256 for drift detection.
- `doctor::Check::*` → each check function takes injectable probes
  (`fn() -> Result<...>`); test PASS / FAIL / SKIP transitions with
  mocked probe results.

### Integration tests (`--dry-run`, no privilege, in `cargo test`)

- `denia setup --dry-run` against a tempdir-rooted InstallContext →
  captures planned steps as a list; assert ordering and idempotency
  markers (presence-of-X → SKIP step).
- `denia uninstall --dry-run [--purge]` → planned `rm` / `systemctl`
  calls match expectations.

### Privileged tests (gated behind `DENIA_RUN_PRIVILEGED_TESTS=1`)

- `tests/cli_setup_privileged.rs` — round-trip on a clean tempdir-rooted
  layout: setup, expect dirs + user + unit + service active; assert perms.
  Marked `#[ignore]`, run via
  `cargo test --test cli_setup_privileged -- --ignored`.
- Same for `uninstall --purge`.

### Manual smoke test (run on a clean VM before tagging release)

1. Fresh Ubuntu 24.04 VM → `git clone … && sudo ./install.sh` → expect
   `/usr/local/bin/denia` present.
2. `sudo denia setup` → expect service active,
   `curl http://127.0.0.1:7180/healthz` returns 200.
3. `denia status` → green.
4. `denia doctor` → all PASS.
5. Edit `~/.config/denia/config.toml` (change `autoscale_interval_s`) →
   `sudo systemctl restart denia` →
   `journalctl -u denia | grep autoscale_interval` shows the new value.
6. `sudo denia rotate-token` → new token printed; `admin.token` differs;
   service still active; old `Authorization: Bearer <old>` → 401, new
   token → 200.
7. `sudo denia uninstall --purge` → service gone, paths gone, user gone.

## Migration

For an existing host that ran today's `install.sh`:

1. Pull the new release; existing service stays up.
2. `sudo ./install.sh --skip-build` (now only rebuilds + reinstalls the
   binary; idempotent on deps).
3. `sudo denia setup` — re-renders the systemd unit, leaves config + keys
   alone (steps 6–8 skip on presence).
4. Done; same data, same admin token, refreshed unit.

Operators on the old `/etc/denia/` layout (pre-ADR-023 rework) need a
one-time manual move of `config.toml`, `admin.token`, `age.key` into
`~/.config/denia/`; `denia doctor` flags the absence.

## Open Questions

- **Should `install.sh` exec `denia setup` at the end?** Argument for: one
  command, zero-friction setup. Argument against: separation of concerns;
  forces operator to acknowledge the post-build setup as a distinct step
  with its own output. Recommendation: print the next-step line, do not
  exec; operator triggers `sudo denia setup` themselves.
- **Drift hash storage:** compute the expected unit hash at build time
  (build.rs writes `unit_hash.in`) or at runtime (re-render template,
  hash). Runtime is simpler and avoids a build script; ship that.
- **Token rotation race:** if a request is in flight during restart, it
  may have authed against the old token but resume against the new
  daemon. Acceptable: the API is request-scoped and reuses the token on
  the next call.

## References

- ADR-023 (TOML config file)
- ADR-021 (Control-plane SOPS secret encryption)
- ADR-020 (Pingora ingress; `:80` / `:443` ownership)
- ADR-005 (Runtime security hardening)
- `install.sh` (current installer)
- `src/main.rs` (daemon entry + existing multi-call dispatch)
- `src/config.rs` (`FileConfig`, `AppConfig::from_env`)
