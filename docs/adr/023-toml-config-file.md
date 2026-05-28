# ADR-023: TOML Config File With Env Override

- Status: Accepted
- Date: 2026-05-28

## Context

Until now, every Denia tunable was read from a `DENIA_*` environment variable
in `AppConfig::from_env`. That works fine under systemd (the unit file in
`install.sh` exports every var explicitly), but it has friction in three
places:

1. Bare-metal/dev setups need a separate shell wrapper or `.env` file to keep
   the env in one place; nothing in the repo enforces a layout.
2. Operators discover available tunables only by reading `src/config.rs` or
   `install.sh`. There is no single "here is the config surface" artefact.
3. First-run UX is hostile: the daemon refuses to boot until
   `DENIA_ADMIN_TOKEN` is exported, so a fresh box always needs an out-of-band
   bootstrap step (the installer does this, but a developer running
   `cargo run` does not).

A file-based configuration with environment-variable override is the standard
shape for daemons of this size and resolves all three.

## Decision

Denia reads operational configuration from a TOML file with environment
variables layered on top.

1. **Path resolution** (first match wins):
   - `$DENIA_CONFIG_FILE` (used by tests and non-standard deployments)
   - `$XDG_CONFIG_HOME/denia/config.toml`
   - `$HOME/.config/denia/config.toml`
   - `/root/.config/denia/config.toml` (systemd-with-`User=root` fallback)
2. **Auto-create on first boot.** If the resolved path does not exist,
   `AppConfig::from_env` writes a fully-populated default template with
   permissions `0600`. The parent directory is `mkdir -p`'d. The template
   includes a freshly generated 32-byte (64 hex char) `admin_token` so the
   daemon can start without operator intervention.
3. **Precedence.** Per field: env var (if set and non-empty) overrides file
   value; file value overrides hardcoded default. The legacy `DENIA_*` env
   contract is therefore preserved verbatim — `install.sh` and existing
   systemd units keep working with zero changes.
4. **Schema.** The TOML schema mirrors the `AppConfig` field set 1:1
   (`bind_addr`, `data_dir`, `http_port`, `https_port`, `acme_email`, the
   OCI cache knobs, autoscale knobs, `age_recipient`, `age_key_file`, …).
   Derived fields (`runtime_dir`, `artifact_dir`, `log_dir`, `database_path`
   default) remain derived from `data_dir` and are not separately
   configurable in the file unless they were already env-controllable.
5. **`deny_unknown_fields`.** Typos in the TOML file are a hard error rather
   than silently ignored.
6. **Comments are not preserved** on rewrite — but the daemon never rewrites
   the file. The auto-generated template carries a one-time header comment.

## Consequences

- Easier: `cargo run` works on a clean box. The first boot creates
  `~/.config/denia/config.toml` with a usable admin token; the operator only
  needs to read that file to learn the token.
- Easier: every tunable is now discoverable in one place — a default
  `config.toml` lists the full surface of operational knobs.
- Easier: env overrides keep CI, container, and `install.sh` workflows
  working unchanged. No migration is required for existing deployments.
- Harder: secret handling. The config file holds the admin token in plain
  text. We mitigate with `0600` perms and the same trust-root expectation as
  `~/.config/denia/age.key` (ADR-021). Production deployments that want to
  keep the token out of the filesystem can continue to export
  `DENIA_ADMIN_TOKEN` from a unit-file `EnvironmentFile=` (env wins).
- Harder: tests that exercise `AppConfig::from_env` must isolate the config
  path. `src/config.rs` does this via a per-test `tempfile::TempDir` plus a
  `FROM_ENV_LOCK` mutex; all `from_env` callers in tests are serialized
  because the env namespace is process-global.

## Alternatives Considered

- **YAML / JSON.** Rejected: TOML is the idiomatic Rust daemon-config
  format, ships in this repo via the `toml` crate (already a transitive
  dep), and tolerates inline comments which are useful for the default
  template's header.
- **`.env` file at the same path.** Rejected: that just shifts the env
  contract to a different filename; it does not give us a typed schema or
  `deny_unknown_fields`.
- **TOML-only, env ignored.** Rejected explicitly during decision-making
  (see commit message): `install.sh`, CI, and per-test overrides all rely
  on env vars; removing that contract would break them.
- **TOML overrides env.** Rejected: surprising. Operators expect
  `DENIA_X=…` to take effect.
- **Leave the admin token blank in the default template and refuse to boot
  until set.** Rejected: that re-introduces the bootstrap-step problem we
  are solving. A randomly generated token in a `0600` file is the better
  default; operators who object can delete it and let the env var win.

## References

- [ADR-021 Control-Plane SOPS Secret Encryption](021-control-plane-secret-encryption.md)
  — same `~/.config/denia/` trust root.
- [`src/config.rs`](../../src/config.rs)
- [`install.sh`](../../install.sh) — env vars still exported by the
  installer; they take precedence over the file.
