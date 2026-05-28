# Denia Binary Subcommands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `install.sh` so it only builds the binary; move all host provisioning (user, dirs, keys, config, systemd unit, start/stop, status, doctor, rotate-token) into `denia <subcommand>` driven by clap.

**Architecture:** Add `src/cli/` with one module per subcommand and a `src/cli/common/` set of single-purpose helpers (privilege, paths, secrets, config_writer, systemd, provision). Extract the existing daemon body into `src/daemon.rs` so `src/main.rs` becomes a thin dispatcher. Add a root `Makefile` so `install.sh` defers all build orchestration to `make install`. Templates for the systemd unit and `config.toml` live under `src/templates/` and are embedded with `include_str!`.

**Tech Stack:** Rust 2024 edition, `clap` v4 (derive), `age` crate for X25519 identity generation, existing `toml`/`rand`/`hex`/`anyhow`/`tokio`/`rustix`/`reqwest`/`sha2` deps. Bash + GNU `make` on the install side. systemd for service lifecycle.

**Spec:** `docs/superpowers/specs/2026-05-28-denia-binary-subcommands-design.md`

---

## File Structure

**New files:**

| Path | Responsibility |
|------|----------------|
| `Makefile` | `make build`, `make install`, `make clean`, etc. Single build entry point. |
| `docs/adr/024-cli-driven-host-provisioning.md` | ADR documenting the split + the rationale. |
| `src/daemon.rs` | Existing `main()` body extracted verbatim into `pub async fn run() -> anyhow::Result<()>`. |
| `src/cli/mod.rs` | `Cli` + `Commands` enums, top-level `dispatch()`. |
| `src/cli/setup.rs` | `denia setup` orchestration. |
| `src/cli/uninstall.rs` | `denia uninstall [--purge]`. |
| `src/cli/status.rs` | `denia status`. |
| `src/cli/doctor.rs` | `denia doctor`. |
| `src/cli/rotate_token.rs` | `denia rotate-token`. |
| `src/cli/common/mod.rs` | Re-exports the helpers below. |
| `src/cli/common/privilege.rs` | `require_root()`, `detect_install_user() -> InstallContext`. |
| `src/cli/common/paths.rs` | `InstallContext` struct + path constants. |
| `src/cli/common/secrets.rs` | `generate_admin_token()`, `generate_age_identity()`. |
| `src/cli/common/config_writer.rs` | `render_config_toml(&InstallContext) -> String`. |
| `src/cli/common/systemd.rs` | `render_unit()`, `write_unit()`, `daemon_reload()`, `enable_now()`, `restart()`, `is_active()`, `wait_active()`. |
| `src/cli/common/provision.rs` | `ensure_user_group()`, `ensure_data_dirs()`, `ensure_cgroup_root()`, `ensure_user_config_dir()`. |
| `src/templates/config.toml.in` | TOML template with `{{placeholder}}` holes. |
| `src/templates/denia.service.in` | systemd unit template with `{{placeholder}}` holes. |
| `tests/cli_dry_run.rs` | Integration tests for `denia setup --dry-run`, `denia uninstall --dry-run`. |
| `tests/cli_setup_privileged.rs` | `#[ignore]`d round-trip on a tempdir-rooted layout, gated by `DENIA_RUN_PRIVILEGED_TESTS=1`. |

**Modified files:**

| Path | Change |
|------|--------|
| `Cargo.toml` | Add `clap = { version = "4", features = ["derive"] }` and `age = "0.10"` (with `armor` feature off). |
| `src/main.rs` | Reduced to: multi-call dispatch → `Cli::parse()` → branch into `cli::dispatch` or `daemon::run`. |
| `src/lib.rs` | Add `pub mod cli;`, `pub mod daemon;` (or whatever the current crate root looks like). |
| `install.sh` | Drop steps 9–12 (provision/secrets/config/unit/start). Step 6 becomes `make install`. Print "Now run: sudo denia setup" at the end. Trim to ~200 lines. |
| `README.md` | Installation section: rewrite to two-step (`sudo ./install.sh` then `sudo denia setup`); add subcommand reference. |
| `docs/adr/023-toml-config-file.md` | Footnote: `denia setup` is now the canonical writer of the operator-home config; update References. |
| `docs/adr/README.md` | Add ADR-024 row. |
| `AGENTS.md` | Reference `denia <subcommand>` for setup/uninstall/status/doctor. |
| `TODO.md` | Update line 10 (install.sh description) to reflect the new responsibilities. |

---

## Conventions

- **Caveman commits**: subject line ≤50 chars, body only when *why* is non-obvious (per repo style).
- **TDD where it pays**: pure functions (secrets, config_writer, render_unit) are test-first. Subcommand orchestration is dry-run-tested first (Task N tests the `--dry-run` plan output, the implementation follows).
- **No phantom imports**: every new module is added to the appropriate `mod`/`pub mod` chain in the same task that creates the file.
- **Run after every step**: `cargo build` after compile-affecting edits; `cargo test -q` after test-affecting edits.
- **Privileged tests stay opt-in**: never enable `DENIA_RUN_PRIVILEGED_TESTS=1` in normal CI flows; gate with `#[ignore]` + helper macros.

---

## Task 1: ADR-024 — CLI-Driven Host Provisioning

**Files:**
- Create: `docs/adr/024-cli-driven-host-provisioning.md`
- Modify: `docs/adr/README.md`

- [ ] **Step 1: Write ADR-024**

Use the same template as recent ADRs (Status, Date, Context, Decision, Consequences, Alternatives Considered, References). Paste:

```markdown
# ADR-024: CLI-Driven Host Provisioning

- Status: Accepted
- Date: 2026-05-28

## Context

`install.sh` (~900 lines) currently owns both build (rustup, node, pnpm, cargo,
distro packages) and provisioning (system user, on-disk layout, age key, admin
token, config.toml, systemd unit, service enable). The provisioning half
duplicates knowledge that already lives in `src/config.rs` and `src/syscall/`,
and the duplication causes drift whenever `FileConfig` gains a field or the
unit template needs to change.

## Decision

Split the installer along the build/provisioning seam:

- `install.sh` keeps preflight, distro package install, rustup, node/pnpm,
  and `make install`. Roughly 200 lines.
- All provisioning lives in `denia <subcommand>` (clap-driven): `setup`,
  `uninstall`, `status`, `doctor`, `rotate-token`. The daemon's run path
  (`denia` with no subcommand) is unchanged.
- A root `Makefile` is the single build entry point. `install.sh`, CI, and
  contributors all call `make build` / `make install`.

The systemd unit content + TOML config schema are emitted from Rust
templates embedded via `include_str!`, so changes track the binary version.
`denia setup` is idempotent: re-run keeps keys + config, refreshes the unit.

## Consequences

- Easier: a new `FileConfig` field is added in one place (`src/config.rs`)
  and reflected by the rendered config; `denia doctor` flags drift.
- Easier: operators get `denia --help`, `denia setup --help`, etc.
- Easier: token rotation is `sudo denia rotate-token` rather than a bespoke
  shell pipeline.
- Harder: binary grows by ~200 KB (clap + age). Acceptable.
- Harder: bootstrap UX is now two commands (`sudo ./install.sh` then
  `sudo denia setup`) rather than one. Operator gets a "next step" hint at
  the end of install.sh.

## Alternatives Considered

- **Keep install.sh as-is.** Rejected: drift cost compounds as the project
  grows; provisioning logic is not bash's strong suit.
- **`install.sh` execs `denia setup` at the end.** Rejected: the two
  phases produce distinct logs and have distinct failure modes; chaining
  them hides the second phase's output behind the first.
- **`cargo xtask` instead of `Makefile`.** Rejected: adds an extra
  workspace member; `make` is universally available and Makefile recipes
  are smaller than xtask Rust glue for the three-step build.

## References

- [Spec: 2026-05-28-denia-binary-subcommands-design.md](../superpowers/specs/2026-05-28-denia-binary-subcommands-design.md)
- ADR-023 (TOML config file)
```

- [ ] **Step 2: Index ADR-024 in the README**

Open `docs/adr/README.md`, add a row at the end of the table that lists ADR-024 with its title.

- [ ] **Step 3: Commit**

```bash
git add docs/adr/024-cli-driven-host-provisioning.md docs/adr/README.md
git commit -m "docs(adr): add ADR-024 cli-driven host provisioning"
```

---

## Task 2: Root Makefile

**Files:**
- Create: `Makefile`

- [ ] **Step 1: Write the Makefile**

```make
SHELL := /usr/bin/env bash

DENIA_BIN := /usr/local/bin/denia
RELEASE_BIN := target/release/denia

.PHONY: all build web rust install clean uninstall help

all: build

help:
	@echo "Targets:"
	@echo "  build       Build web SPA then cargo build --release --locked"
	@echo "  web         Build web/dist/client only"
	@echo "  rust        Run cargo build --release --locked only"
	@echo "  install     build + copy binary to $(DENIA_BIN) (requires root)"
	@echo "  clean       cargo clean + remove web/dist + web/node_modules"
	@echo "  uninstall   rm -f $(DENIA_BIN) (requires root)"

build: web rust

web:
	cd web && pnpm install --frozen-lockfile && pnpm build

rust:
	cargo build --release --locked

install: build
	install -Dm0755 $(RELEASE_BIN) $(DENIA_BIN)

clean:
	cargo clean
	rm -rf web/dist web/node_modules

uninstall:
	rm -f $(DENIA_BIN)
```

- [ ] **Step 2: Verify targets parse**

Run: `make -n build`
Expected: prints `cd web && pnpm install ...` and `cargo build --release --locked`, no execution.

- [ ] **Step 3: Verify `make help` works**

Run: `make help`
Expected: lists every target.

- [ ] **Step 4: Commit**

```bash
git add Makefile
git commit -m "build: add root Makefile as single build entry point"
```

---

## Task 3: Add `clap` + `age` to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the two crates**

In the `[dependencies]` block (preserving alphabetical/grouped style):

```toml
clap = { version = "4", features = ["derive"] }
age = { version = "0.10", default-features = false }
```

- [ ] **Step 2: Verify the build is still green**

Run: `cargo build --release --locked` (or omit `--locked` if `Cargo.lock` regenerates)
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add clap + age for cli subcommands"
```

---

## Task 4: Extract Daemon Body Into `src/daemon.rs`

**Files:**
- Create: `src/daemon.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs` (if it exists; otherwise leave `src/main.rs` alone except for the change below)

- [ ] **Step 1: Move `async fn main()`'s body into a new function**

Create `src/daemon.rs`:

```rust
//! Daemon entry point extracted from src/main.rs so the CLI dispatcher can
//! branch into it without re-implementing the boot sequence.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::{
    app::{AppState, build_router},
    config::AppConfig,
    ingress::pingora::{
        AcmeDriver, ChallengeStore, IngressServerConfig, RENEWAL_WINDOW_DAYS, build_server,
        load_certs_from_disk, persist_cert, run_server, select_renewals,
    },
    scheduler::{Scheduler, run_until_shutdown},
    state::SqliteStore,
};

/// Run the full denia daemon (control plane + ingress + scheduler + ACME +
/// autoscaler + OCI GC). Called when the binary is invoked with no
/// subcommand.
pub async fn run() -> anyhow::Result<()> {
    // <PASTE THE EXACT CURRENT BODY OF src/main.rs FROM `let config = AppConfig::from_env()?;` THROUGH `Ok(())` HERE>
    todo!("paste current body verbatim then delete this line")
}

// <PASTE the existing free fns `issue_missing_certs` and `reissue` here as
// private fns of this module; they currently sit in main.rs.>
```

(The "paste" placeholders make this step concrete. The aim is byte-for-byte
relocation, not refactor.)

- [ ] **Step 2: Hook the module into the crate**

If `src/lib.rs` exists, add `pub mod daemon;`. If not, add `mod daemon;` to
the top of `src/main.rs` and reference it as `daemon::run()`.

- [ ] **Step 3: Shrink `src/main.rs` to call into the new module**

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Multi-call (existing): socket-proxy / workload-launcher.
    let mut args = std::env::args_os();
    let argv0 = args.next();
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "socket-proxy")
    {
        denia::socket_proxy::run_from_args(args).await?;
        return Ok(());
    }
    if argv0
        .as_ref()
        .and_then(|path| std::path::Path::new(path).file_name())
        .is_some_and(|name| name == "workload-launcher")
    {
        let code = denia::workload_launcher::run_from_args(args)?;
        std::process::exit(code);
    }

    // Default (no subcommand for now; clap dispatch added in Task 6).
    denia::daemon::run().await
}
```

- [ ] **Step 4: Build + run the existing tests**

Run: `cargo build && cargo test`
Expected: green. No behavior change.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/daemon.rs src/lib.rs
git commit -m "refactor: extract daemon body to daemon::run"
```

---

## Task 5: Add `InstallContext` + Path Helpers

**Files:**
- Create: `src/cli/mod.rs` (initially: `pub mod common;` and module skeleton)
- Create: `src/cli/common/mod.rs`
- Create: `src/cli/common/paths.rs`
- Create: `src/cli/common/privilege.rs`
- Modify: `src/lib.rs` to add `pub mod cli;`
- Test: `src/cli/common/paths.rs` (inline `#[cfg(test)]` mod)

- [ ] **Step 1: Write the InstallContext test**

In `src/cli/common/paths.rs`:

```rust
//! Resolved per-install paths. Built once at the top of every privileged
//! subcommand from `$SUDO_USER` + `getent passwd`.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallContext {
    pub install_user: String,
    pub install_home: PathBuf,
    pub user_config_dir: PathBuf,
    pub config_file: PathBuf,
    pub token_file: PathBuf,
    pub age_key_file: PathBuf,
}

impl InstallContext {
    /// Build paths from the installing operator's identity. Used in tests
    /// and behind `privilege::detect_install_user`.
    pub fn from_user(user: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        let install_user = user.into();
        let install_home = home.into();
        let user_config_dir = install_home.join(".config/denia");
        Self {
            install_user,
            install_home,
            config_file: user_config_dir.join("config.toml"),
            token_file: user_config_dir.join("admin.token"),
            age_key_file: user_config_dir.join("age.key"),
            user_config_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_derived_from_home() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        assert_eq!(ctx.install_user, "rakei");
        assert_eq!(ctx.install_home, PathBuf::from("/home/rakei"));
        assert_eq!(ctx.user_config_dir, PathBuf::from("/home/rakei/.config/denia"));
        assert_eq!(ctx.config_file, PathBuf::from("/home/rakei/.config/denia/config.toml"));
        assert_eq!(ctx.token_file, PathBuf::from("/home/rakei/.config/denia/admin.token"));
        assert_eq!(ctx.age_key_file, PathBuf::from("/home/rakei/.config/denia/age.key"));
    }
}
```

- [ ] **Step 2: Write `privilege.rs`**

```rust
//! Privilege + installer-identity detection used by every privileged
//! subcommand. Refuses to run unless invoked via `sudo` from a non-root
//! account.

use anyhow::{Context, anyhow};
use std::path::PathBuf;
use std::process::Command;

use super::paths::InstallContext;

pub fn require_root() -> anyhow::Result<()> {
    // SAFETY: getuid is signal-safe and never fails.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        return Err(anyhow!(
            "this subcommand must run as root (try: sudo denia ...)"
        ));
    }
    Ok(())
}

pub fn detect_install_user() -> anyhow::Result<InstallContext> {
    let user = std::env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")
        .ok_or_else(|| anyhow!(
            "this subcommand must be invoked via sudo from a non-root account; \
             SUDO_USER is unset or = 'root'"
        ))?;

    let home = resolve_home(&user)?;
    Ok(InstallContext::from_user(user, home))
}

fn resolve_home(user: &str) -> anyhow::Result<PathBuf> {
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .with_context(|| format!("invoking getent passwd {user}"))?;
    if !output.status.success() {
        return Err(anyhow!("getent passwd {user} returned non-zero"));
    }
    let line = String::from_utf8(output.stdout).context("getent output not utf-8")?;
    let home = line
        .trim_end_matches('\n')
        .split(':')
        .nth(5)
        .ok_or_else(|| anyhow!("malformed passwd line for {user}: {line:?}"))?
        .to_string();
    if home.is_empty() {
        return Err(anyhow!("empty HOME for {user}"));
    }
    Ok(PathBuf::from(home))
}
```

- [ ] **Step 3: Wire `cli::common` exports**

`src/cli/common/mod.rs`:

```rust
pub mod paths;
pub mod privilege;

pub use paths::InstallContext;
pub use privilege::{detect_install_user, require_root};
```

`src/cli/mod.rs`:

```rust
//! Subcommand surface for the denia binary. See ADR-024 + spec
//! 2026-05-28-denia-binary-subcommands-design.md.

pub mod common;
```

Add `pub mod cli;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -q --lib cli::common::paths`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli src/lib.rs
git commit -m "cli: add InstallContext + privilege detection scaffolding"
```

---

## Task 6: Wire `clap` Dispatcher + `denia version`

**Files:**
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Define `Cli` + `Commands` in `src/cli/mod.rs`**

Append below the existing module declarations:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "denia",
    version = env!("CARGO_PKG_VERSION"),
    about = "Denia single-node PaaS",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Provision the host: user, dirs, keys, config, systemd unit, start.
    Setup,
    /// Tear down the service. With --purge: also wipe data + user + config.
    Uninstall {
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Print live service state.
    Status,
    /// Diagnose host + denia install.
    Doctor,
    /// Regenerate the admin token and restart the service.
    RotateToken,
}

/// Entry point called from main.rs when the binary was not invoked as a
/// multi-call (socket-proxy / workload-launcher) and a subcommand is
/// present.
pub async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Commands::Setup) => Err(anyhow::anyhow!("setup not yet implemented")),
        Some(Commands::Uninstall { .. }) => Err(anyhow::anyhow!("uninstall not yet implemented")),
        Some(Commands::Status) => Err(anyhow::anyhow!("status not yet implemented")),
        Some(Commands::Doctor) => Err(anyhow::anyhow!("doctor not yet implemented")),
        Some(Commands::RotateToken) => Err(anyhow::anyhow!("rotate-token not yet implemented")),
        None => crate::daemon::run().await,
    }
}
```

(The placeholder `Err(...)` returns get replaced in subsequent tasks; clap
already gives us `--help` + `--version`.)

- [ ] **Step 2: Use `Cli::parse()` in `src/main.rs`**

Replace the post-multi-call body with:

```rust
    let cli = denia::cli::Cli::parse();
    denia::cli::dispatch(cli).await
```

(Keep the multi-call block above unchanged.)

- [ ] **Step 3: Verify**

```bash
cargo build
./target/debug/denia --version
./target/debug/denia --help
./target/debug/denia setup    # expect "setup not yet implemented"
```

- [ ] **Step 4: Run the test suite**

Run: `cargo test`
Expected: green; nothing depends on the dispatcher yet.

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "cli: wire clap dispatcher with version/help"
```

---

## Task 7: `secrets` Module — Admin Token Generation (TDD)

**Files:**
- Create: `src/cli/common/secrets.rs`
- Modify: `src/cli/common/mod.rs` (add `pub mod secrets;`)

- [ ] **Step 1: Write the failing test**

```rust
//! Secret material generated during `denia setup` and `denia rotate-token`.
//! Tokens are 32 random bytes -> 64 hex chars (matches FileConfig's >=64
//! floor in src/config.rs).

use anyhow::Context;
use rand::RngExt;

/// Generate a 32-byte random token rendered as 64 lowercase hex chars.
pub fn generate_admin_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_token_is_64_hex_chars() {
        let t = generate_admin_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())));
    }

    #[test]
    fn admin_tokens_differ_across_calls() {
        let a = generate_admin_token();
        let b = generate_admin_token();
        assert_ne!(a, b);
    }
}
```

Add `pub mod secrets;` to `src/cli/common/mod.rs`.

- [ ] **Step 2: Run + verify tests pass**

```bash
cargo test -q --lib cli::common::secrets
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/cli/common/secrets.rs src/cli/common/mod.rs
git commit -m "cli: add admin token generator"
```

---

## Task 8: `secrets` Module — Age Identity Generation (TDD)

**Files:**
- Modify: `src/cli/common/secrets.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/cli/common/secrets.rs`:

```rust
/// Mint a fresh X25519 age identity and serialize it in the same format
/// `age-keygen` writes (a `# public key:` comment line + the
/// `AGE-SECRET-KEY-1...` body line). `FileConfig::read_age_public_key`
/// parses this format to auto-derive the recipient.
pub fn generate_age_identity() -> String {
    let id = age::x25519::Identity::generate();
    let pub_key = id.to_public();
    format!("# public key: {pub}\n{secret}\n", pub = pub_key, secret = id.to_string().expose_secret())
}

#[cfg(test)]
mod age_tests {
    use super::*;

    #[test]
    fn age_identity_has_public_key_comment() {
        let s = generate_age_identity();
        assert!(s.contains("# public key: age1"), "missing public key comment in:\n{s}");
        assert!(s.contains("AGE-SECRET-KEY-1"), "missing secret body in:\n{s}");
    }

    #[test]
    fn age_identity_parses_back() {
        let s = generate_age_identity();
        // Last non-empty line should parse as a valid age::x25519::Identity.
        let secret_line = s
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .next()
            .unwrap();
        let parsed: age::x25519::Identity = secret_line.parse().expect("identity parses");
        // Round-trip: public key from parsed identity should match what we wrote.
        assert!(s.contains(&format!("# public key: {}", parsed.to_public())));
    }
}
```

(Note: the `age::x25519::Identity::to_string()` returns a
`secrecy::SecretString`; we import the `ExposeSecret` trait inline. If the
crate's API has shifted, adjust the snippet to use the current
`age = "0.10"` API. Verify against `age` docs before pasting.)

- [ ] **Step 2: Run the tests**

```bash
cargo test -q --lib cli::common::secrets
```

Expected: PASS (all four tests).

- [ ] **Step 3: Commit**

```bash
git add src/cli/common/secrets.rs
git commit -m "cli: add age identity generator"
```

---

## Task 9: `config.toml.in` Template + `config_writer` (TDD)

**Files:**
- Create: `src/templates/config.toml.in`
- Create: `src/cli/common/config_writer.rs`
- Modify: `src/cli/common/mod.rs` (add `pub mod config_writer;`)

- [ ] **Step 1: Write the template**

Create `src/templates/config.toml.in` mirroring the bash-emitted TOML
from the current `install.sh::write_config_file`. Use `{{user_home}}`,
`{{user_config_dir}}`, `{{age_key_file}}` placeholders. Keep comments.

- [ ] **Step 2: Write the failing test**

```rust
//! Renders the operator's `~/.config/denia/config.toml` from an
//! InstallContext. Plain string substitution; the source of the template
//! is `src/templates/config.toml.in` embedded via include_str!.

use super::paths::InstallContext;
use crate::config::FileConfig;

const TEMPLATE: &str = include_str!("../../templates/config.toml.in");

pub fn render_config_toml(ctx: &InstallContext) -> String {
    TEMPLATE
        .replace("{{user_config_dir}}", &ctx.user_config_dir.display().to_string())
        .replace("{{age_key_file}}", &ctx.age_key_file.display().to_string())
        .replace("{{token_file}}", &ctx.token_file.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_config_parses_as_file_config() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let toml_str = render_config_toml(&ctx);
        let parsed: FileConfig = toml::from_str(&toml_str)
            .expect("rendered config.toml must parse as FileConfig");
        assert_eq!(parsed.age_key_file.as_deref(), Some(ctx.age_key_file.as_path()));
    }

    #[test]
    fn rendered_config_contains_expected_paths() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let toml_str = render_config_toml(&ctx);
        assert!(toml_str.contains("/home/rakei/.config/denia/age.key"));
    }
}
```

(Resolve the `include_str!` path relative to `src/cli/common/config_writer.rs`.
If the file lives at `src/templates/config.toml.in`, the relative path is
`../../templates/config.toml.in`. Verify before pasting.)

- [ ] **Step 3: Run + verify tests pass**

```bash
cargo test -q --lib cli::common::config_writer
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/templates/config.toml.in src/cli/common/config_writer.rs src/cli/common/mod.rs
git commit -m "cli: add config.toml template + config_writer"
```

---

## Task 10: `denia.service.in` Template + `systemd::render_unit` (TDD)

**Files:**
- Create: `src/templates/denia.service.in`
- Create: `src/cli/common/systemd.rs`
- Modify: `src/cli/common/mod.rs` (add `pub mod systemd;`)

- [ ] **Step 1: Write the template**

Create `src/templates/denia.service.in` mirroring the unit text emitted by
`install.sh::write_systemd_unit` *after* the operator-home rework already
landed. Placeholders: `{{denia_bin}}`, `{{denia_user}}`, `{{denia_group}}`,
`{{denia_home}}`, `{{config_file}}`, `{{age_key_file}}`, `{{token_file}}`,
`{{user_config_dir}}`.

- [ ] **Step 2: Write the failing test**

```rust
//! systemd unit rendering + thin wrappers around `systemctl`.

use super::paths::InstallContext;
use sha2::Digest;

const TEMPLATE: &str = include_str!("../../templates/denia.service.in");

pub fn render_unit(ctx: &InstallContext) -> String {
    TEMPLATE
        .replace("{{denia_bin}}", "/usr/local/bin/denia")
        .replace("{{denia_user}}", "denia")
        .replace("{{denia_group}}", "denia")
        .replace("{{denia_home}}", "/var/lib/denia")
        .replace("{{config_file}}", &ctx.config_file.display().to_string())
        .replace("{{age_key_file}}", &ctx.age_key_file.display().to_string())
        .replace("{{token_file}}", &ctx.token_file.display().to_string())
        .replace("{{user_config_dir}}", &ctx.user_config_dir.display().to_string())
}

pub fn unit_sha256(ctx: &InstallContext) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(render_unit(ctx).as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_contains_required_directives() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let unit = render_unit(&ctx);
        for needle in [
            "User=denia",
            "Group=denia",
            "Environment=DENIA_CONFIG_FILE=/home/rakei/.config/denia/config.toml",
            "Environment=SOPS_AGE_KEY_FILE=/home/rakei/.config/denia/age.key",
            "EnvironmentFile=/home/rakei/.config/denia/admin.token",
            "ExecStart=/usr/local/bin/denia",
            "BindReadOnlyPaths=/home/rakei/.config/denia",
            "AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN CAP_SETUID CAP_SETGID",
            "Delegate=yes",
        ] {
            assert!(unit.contains(needle), "expected `{needle}` in unit:\n{unit}");
        }
    }

    #[test]
    fn unit_hash_is_stable_for_same_inputs() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        assert_eq!(unit_sha256(&ctx), unit_sha256(&ctx));
    }
}
```

- [ ] **Step 3: Run + verify tests pass**

```bash
cargo test -q --lib cli::common::systemd
```

- [ ] **Step 4: Commit**

```bash
git add src/templates/denia.service.in src/cli/common/systemd.rs src/cli/common/mod.rs
git commit -m "cli: add systemd unit template + render_unit"
```

---

## Task 11: `systemd` — `systemctl` Wrappers

**Files:**
- Modify: `src/cli/common/systemd.rs`

- [ ] **Step 1: Append `systemctl` wrappers**

```rust
use std::process::{Command, Stdio};

pub fn write_unit(ctx: &InstallContext) -> anyhow::Result<()> {
    let path = "/etc/systemd/system/denia.service";
    let body = render_unit(ctx);
    // Atomic write: tmp + fsync + rename.
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
    Ok(())
}

pub fn daemon_reload() -> anyhow::Result<()> {
    run("systemctl", &["daemon-reload"])
}

pub fn enable_now(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["enable", "--now", unit])
}

pub fn restart(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["restart", unit])
}

pub fn disable_now(unit: &str) -> anyhow::Result<()> {
    run("systemctl", &["disable", "--now", unit])
}

pub fn is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn wait_active(unit: &str, timeout: std::time::Duration) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if is_active(unit) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(anyhow::anyhow!("{unit} did not become active in {timeout:?}"))
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("{bin} {args:?} exited {status}"));
    }
    Ok(())
}
```

(Adjust the `set_permissions` import for `PermissionsExt::from_mode` — use
`std::fs::Permissions::from_mode(0o644)` via the unix extension; the
snippet sketches intent, not final compilable code.)

- [ ] **Step 2: Build**

```bash
cargo build
```

- [ ] **Step 3: Commit**

```bash
git add src/cli/common/systemd.rs
git commit -m "cli: add systemctl helpers"
```

---

## Task 12: `provision` — User, Group, Dirs

**Files:**
- Create: `src/cli/common/provision.rs`
- Modify: `src/cli/common/mod.rs` (add `pub mod provision;`)

- [ ] **Step 1: Implement idempotent provisioning helpers**

```rust
//! User/group/directory provisioning used by `denia setup`. Every helper
//! is a probe-then-act idempotent operation so re-runs are safe.

use std::path::Path;
use std::process::Command;

use super::paths::InstallContext;

pub fn ensure_group(name: &str) -> anyhow::Result<bool> {
    if group_exists(name)? {
        return Ok(false);
    }
    run("groupadd", &["--system", name])?;
    Ok(true)
}

pub fn ensure_user(user: &str, group: &str, home: &str) -> anyhow::Result<bool> {
    if user_exists(user)? {
        return Ok(false);
    }
    run("useradd", &[
        "--system",
        "--gid", group,
        "--home-dir", home,
        "--no-create-home",
        "--shell", "/usr/sbin/nologin",
        user,
    ])?;
    Ok(true)
}

pub fn ensure_dir(path: &Path, mode: u32, owner: &str, group: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    run("chmod", &[&format!("{mode:o}"), &path.display().to_string()])?;
    run("chown", &[&format!("{owner}:{group}"), &path.display().to_string()])?;
    Ok(())
}

pub fn ensure_data_dirs() -> anyhow::Result<()> {
    let base = std::path::Path::new("/var/lib/denia");
    for sub in ["", "sqlite", "artifacts", "tls", "runtime", "logs"] {
        let p = base.join(sub);
        ensure_dir(&p, 0o700, "denia", "denia")?;
    }
    Ok(())
}

pub fn ensure_cgroup_root() -> anyhow::Result<()> {
    let p = std::path::Path::new("/sys/fs/cgroup/denia");
    ensure_dir(p, 0o755, "denia", "denia")
}

pub fn ensure_user_config_dir(ctx: &InstallContext) -> anyhow::Result<()> {
    // ~/.config (may exist; ensure permissions but don't change owner if it does).
    let parent = ctx.user_config_dir.parent().unwrap();
    if !parent.exists() {
        ensure_dir(parent, 0o700, &ctx.install_user, &ctx.install_user)?;
    }
    // The denia subdir.
    ensure_dir(&ctx.user_config_dir, 0o750, &ctx.install_user, "denia")?;
    Ok(())
}

fn group_exists(name: &str) -> anyhow::Result<bool> {
    let s = Command::new("getent").args(["group", name]).status()?;
    Ok(s.success())
}

fn user_exists(name: &str) -> anyhow::Result<bool> {
    let s = Command::new("getent").args(["passwd", name]).status()?;
    Ok(s.success())
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin).args(args).status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("{bin} {args:?} exited {status}"));
    }
    Ok(())
}
```

- [ ] **Step 2: Add to `cli::common`**

In `src/cli/common/mod.rs`: `pub mod provision;`.

- [ ] **Step 3: Build**

```bash
cargo build
```

- [ ] **Step 4: Commit**

```bash
git add src/cli/common/provision.rs src/cli/common/mod.rs
git commit -m "cli: add provisioning helpers"
```

---

## Task 13: `denia setup` Implementation + Dry-Run

**Files:**
- Create: `src/cli/setup.rs`
- Modify: `src/cli/mod.rs` (add `pub mod setup;`, wire dispatch, accept `SetupArgs`)
- Test: `tests/cli_dry_run.rs`

- [ ] **Step 1: Define `SetupArgs`**

In `src/cli/mod.rs`, change the `Setup` variant to:

```rust
    Setup(SetupArgs),
```

with `pub struct SetupArgs { #[arg(long)] pub dry_run: bool }` defined
near the top.

- [ ] **Step 2: Write `setup::run`**

```rust
use anyhow::Context;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::common::{
    config_writer, paths::InstallContext, privilege, provision, secrets, systemd,
};

#[derive(clap::Args, Debug)]
pub struct SetupArgs {
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    privilege::require_root()?;
    let ctx = privilege::detect_install_user()?;

    let plan = build_plan(&ctx);
    for step in &plan {
        if args.dry_run {
            println!("[dry-run] {}", step.label());
        } else {
            step.execute(&ctx)?;
        }
    }

    if !args.dry_run {
        print_summary(&ctx);
    }
    Ok(())
}

enum Step { /* one variant per setup step; build_plan returns Vec<Step> */ }

impl Step {
    fn label(&self) -> &'static str { /* … */ }
    fn execute(&self, ctx: &InstallContext) -> anyhow::Result<()> { /* … */ }
}

fn build_plan(ctx: &InstallContext) -> Vec<Step> { /* … */ }

fn print_summary(ctx: &InstallContext) { /* … */ }
```

Flesh out `Step`'s 12 variants exactly mirroring the spec's setup steps (each
delegating to `provision::*`, `secrets::*`, `config_writer`, `systemd::*`).
Skip-if-present behavior is encoded in `execute`.

- [ ] **Step 3: Wire dispatch**

In `src/cli/mod.rs::dispatch`:

```rust
        Some(Commands::Setup(args)) => crate::cli::setup::run(args),
```

(Note: blocking helper; either call from inside `tokio::task::spawn_blocking`
or restructure `dispatch` to dispatch sync subcommands via
`tokio::runtime::Handle::block_on` only for the daemon variant. Keep
subcommand handlers sync; only `daemon::run` is async.)

Refactor `dispatch` to:

```rust
pub fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Commands::Setup(args))           => crate::cli::setup::run(args),
        Some(Commands::Uninstall(args))       => crate::cli::uninstall::run(args),
        Some(Commands::Status)                => crate::cli::status::run(),
        Some(Commands::Doctor)                => crate::cli::doctor::run(),
        Some(Commands::RotateToken)           => crate::cli::rotate_token::run(),
        None => {
            // Daemon is async; build a runtime here.
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::daemon::run())
        }
    }
}
```

Drop `#[tokio::main]` from `src/main.rs::main` and remove the `.await`s
there.

- [ ] **Step 4: Integration test — dry-run plan**

`tests/cli_dry_run.rs`:

```rust
use assert_cmd::Command;

#[test]
fn setup_dry_run_lists_expected_steps() {
    let output = Command::cargo_bin("denia")
        .unwrap()
        .env("SUDO_USER", "rakei")
        // For dry-run, require_root check is skipped only if EUID==0; this test
        // therefore covers a unit-level Plan builder. Adapt as needed.
        .args(["setup", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in ["groupadd", "useradd", "/var/lib/denia", "config.toml", "systemctl"] {
        assert!(stdout.contains(needle), "missing `{needle}` in:\n{stdout}");
    }
}
```

(If `assert_cmd` is not yet a dev-dependency, add it in Step 1 of this task
and commit alongside.)

If the EUID guard is too strict for the test, move the plan-building logic
into a pure function and unit-test it via the `tests/cli_dry_run.rs` file
without requiring the binary to be invoked under root.

- [ ] **Step 5: Run + verify**

```bash
cargo test -q
sudo target/debug/denia setup --dry-run     # smoke-test on the dev box
```

- [ ] **Step 6: Commit**

```bash
git add src/cli/setup.rs src/cli/mod.rs src/main.rs tests/cli_dry_run.rs Cargo.toml Cargo.lock
git commit -m "cli: implement denia setup with --dry-run"
```

---

## Task 14: `denia uninstall` Implementation

**Files:**
- Create: `src/cli/uninstall.rs`
- Modify: `src/cli/mod.rs` (add `pub mod uninstall;`)
- Modify: `tests/cli_dry_run.rs` (add uninstall dry-run case)

- [ ] **Step 1: Implement `uninstall::run`**

Mirror the setup pattern: build a `Vec<Step>`, dry-run or execute. Steps:
disable+stop service; remove unit file; daemon-reload; with `--purge`:
remove `/var/lib/denia`, `~/.config/denia`, userdel, groupdel,
`rmdir /sys/fs/cgroup/denia`. Do not remove the running binary.

```rust
#[derive(clap::Args, Debug)]
pub struct UninstallArgs {
    #[arg(long)] pub purge: bool,
    #[arg(long)] pub dry_run: bool,
}

pub fn run(args: UninstallArgs) -> anyhow::Result<()> { /* … */ }
```

Add dispatch wiring.

- [ ] **Step 2: Extend dry-run integration test**

```rust
#[test]
fn uninstall_dry_run_lists_expected_steps() {
    let output = Command::cargo_bin("denia").unwrap()
        .env("SUDO_USER", "rakei")
        .args(["uninstall", "--dry-run", "--purge"])
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in ["systemctl disable", "/etc/systemd/system/denia.service", "userdel denia", "rm -rf /var/lib/denia"] {
        assert!(stdout.contains(needle), "missing `{needle}` in:\n{stdout}");
    }
}
```

- [ ] **Step 3: Run + verify**

```bash
cargo test -q
```

- [ ] **Step 4: Commit**

```bash
git add src/cli/uninstall.rs src/cli/mod.rs tests/cli_dry_run.rs
git commit -m "cli: implement denia uninstall with --dry-run + --purge"
```

---

## Task 15: `denia status`

**Files:**
- Create: `src/cli/status.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Implement `status::run`**

- Read the resolved config file path (`DENIA_CONFIG_FILE` env or default
  search chain via `crate::config::config_file_path`).
- Parse the `bind_addr` field from the TOML if the file exists; else use
  `127.0.0.1:7180`.
- Probe `systemctl is-active denia.service`.
- Probe `http://<bind_addr>/healthz` with a 2s timeout using `reqwest`.
- Print the report.
- Exit 0 iff service is active AND health check is 200.

- [ ] **Step 2: Commit**

```bash
git add src/cli/status.rs src/cli/mod.rs
git commit -m "cli: implement denia status"
```

---

## Task 16: `denia doctor`

**Files:**
- Create: `src/cli/doctor.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Implement `doctor::run`** with the seven checks named in
the spec. Each check returns `enum Check { Pass, Fail(String), Skip(String) }`.

- [ ] **Step 2: Drift-detection check**

Re-render the systemd unit from the current `InstallContext`, hash it, hash
the on-disk `/etc/systemd/system/denia.service` content, compare.

- [ ] **Step 3: Commit**

```bash
git add src/cli/doctor.rs src/cli/mod.rs
git commit -m "cli: implement denia doctor"
```

---

## Task 17: `denia rotate-token`

**Files:**
- Create: `src/cli/rotate_token.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Implement `rotate_token::run`**

- `require_root + detect_install_user`.
- Generate a fresh token via `secrets::generate_admin_token`.
- Write `<token_file>.tmp`, fsync, rename over the original; ensure
  `0640 <user>:denia`.
- `systemd::restart("denia.service")`.
- `systemd::wait_active("denia.service", Duration::from_secs(15))`.
- Print the new token to stdout once.

- [ ] **Step 2: Commit**

```bash
git add src/cli/rotate_token.rs src/cli/mod.rs
git commit -m "cli: implement denia rotate-token"
```

---

## Task 18: Refactor `install.sh`

**Files:**
- Modify: `install.sh`

- [ ] **Step 1: Trim install.sh**

Remove `step_create_user`, `step_create_layout`, `step_age_key`,
`step_admin_token`, `step_config_file`, `step_systemd_unit`,
`step_start_service`, `step_summary`, `detect_install_user`,
`write_config_file`, `write_systemd_unit`, and their callsites.

Replace `step_build_rust` + `step_build_web` with a single
`step_make_install` that runs `make install` (the Makefile orchestrates
both builds + binary copy).

The final `main()` becomes:

```bash
main() {
    if [[ "${UNINSTALL}" -eq 1 ]]; then
        fail "Use 'sudo denia uninstall' instead of install.sh --uninstall."
    fi
    step_preflight_os
    step_preflight_kernel
    step_preflight_ports
    step_install_prereqs
    step_install_rust
    step_install_node
    step_make_install
    print_next_step
}
```

`print_next_step` prints: "Build complete. Now run: sudo denia setup".

- [ ] **Step 2: Drop `--uninstall` flag from install.sh's CLI help**

Replace usage block to advertise only `--dry-run` and `--skip-build`.

- [ ] **Step 3: Verify with `--dry-run`**

```bash
sudo ./install.sh --dry-run
```

Expected: preflight + package install + rustup + node + `make install` +
"Now run: sudo denia setup".

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "install: shrink install.sh to build-only; defer provisioning to denia setup"
```

---

## Task 19: Privileged Integration Test (Gated)

**Files:**
- Create: `tests/cli_setup_privileged.rs`

- [ ] **Step 1: Skeleton + gating**

```rust
//! Round-trip the `denia setup` happy path against a tempdir-rooted
//! layout. Requires root + cgroup v2 + userns. Opt-in via
//! `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test cli_setup_privileged -- --ignored`.

#[test]
#[ignore]
fn setup_creates_user_dirs_unit_and_starts_service() {
    if std::env::var("DENIA_RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping privileged test; set DENIA_RUN_PRIVILEGED_TESTS=1");
        return;
    }
    // Run binary, assert user + paths + unit + active service. Clean up.
    todo!();
}
```

- [ ] **Step 2: Commit**

```bash
git add tests/cli_setup_privileged.rs
git commit -m "test: gated privileged integration test for denia setup"
```

---

## Task 20: Update README + ADR-023 + AGENTS.md + TODO.md

**Files:**
- Modify: `README.md`
- Modify: `docs/adr/023-toml-config-file.md`
- Modify: `AGENTS.md`
- Modify: `TODO.md`

- [ ] **Step 1: README — rewrite Installation section**

Replace the existing "Installation" section with:

- "Build + install binary": `git clone … && sudo ./install.sh`
- "Provision the host": `sudo denia setup`
- Path table (unchanged from earlier rework, but framed as produced by
  `denia setup`)
- Reference `denia --help`, `denia status`, `denia doctor`, `denia
  rotate-token`, `denia uninstall`

- [ ] **Step 2: ADR-023 references**

Add a one-line footnote: "Provisioning is done by `denia setup` (ADR-024),
which writes the file using the same paths defined here."

- [ ] **Step 3: AGENTS.md operators section**

Reference `denia setup` / `denia status` / `denia doctor` where the
project-conventions section talks about installer flow.

- [ ] **Step 4: TODO.md line 10**

Update to describe install.sh as build-only and `denia setup` as the
provisioning command.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/adr/023-toml-config-file.md AGENTS.md TODO.md
git commit -m "docs: align README/ADR-023/AGENTS/TODO with cli-driven setup"
```

---

## Task 21: Full Test Pass + Smoke Test

**Files:** none (verification only).

- [ ] **Step 1: Run the unit + integration suite**

```bash
cargo fmt --all
cargo build
cargo test
cargo clippy --all-targets --all-features
```

Expected: all green.

- [ ] **Step 2: Run the privileged integration test on a dev box**

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test cli_setup_privileged -- --ignored
```

Expected: PASS or skipped with a clear message.

- [ ] **Step 3: Manual smoke test on a clean VM**

Run the seven-step smoke test from the spec (Section "Manual smoke test").
Each step must pass before merging.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore: format pass + smoke-test fixes"
```

(If no changes, skip the commit.)

---

## Verification Commands

| Command | Expected |
|---------|----------|
| `cargo build` | Compiles cleanly with new clap + age deps. |
| `cargo test` | All unit + dry-run integration tests pass. |
| `cargo clippy --all-targets --all-features` | No new warnings. |
| `make help` | Lists every Makefile target. |
| `./target/debug/denia --help` | clap prints the subcommand surface. |
| `./target/debug/denia version` | Prints version + git SHA. |
| `sudo target/debug/denia setup --dry-run` | Prints the 12-step plan, no side effects. |
| `sudo target/debug/denia uninstall --dry-run --purge` | Prints the teardown plan, no side effects. |
| `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test cli_setup_privileged -- --ignored` | Round-trips on a dev VM. |

---

## Open Risks Tracked During Implementation

- **`age = "0.10"` API surface** — the snippet in Task 8 uses
  `Identity::generate` + `.to_public()` + `.to_string().expose_secret()`.
  If the actual 0.10 API differs, adjust accordingly; do not silently
  reformat the serialized identity (`FileConfig::read_age_public_key`
  expects the exact `# public key: …` line `age-keygen` produces).
- **`block_on` vs `#[tokio::main]`** — Task 13 Step 3 switches the binary
  from `#[tokio::main]` to an explicit `tokio::runtime::Runtime` built
  inside the `None` arm. Verify that nothing in the daemon path requires
  the multi-threaded macro's `worker_threads` defaults; if it does, build
  the runtime with `Builder::new_multi_thread().enable_all()`.
- **Permission-set ordering during `denia setup`** — the daemon needs
  to read `~/.config/denia/*` via the `denia` group, which means
  `chgrp denia` must happen *after* `chown <user>` and *before*
  attempting to start the service. The `provision::ensure_dir` helper
  enforces order; the per-file write paths must do the same.
