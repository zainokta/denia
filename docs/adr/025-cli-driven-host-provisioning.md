# ADR-025: CLI-Driven Host Provisioning

- Status: Accepted
- Date: 2026-05-28

## Context

`install.sh` (~900 lines) currently owns both build (rustup, node, pnpm, cargo, distro packages) and provisioning (system user, on-disk layout, age key, admin token, config.toml, systemd unit, service enable). The provisioning half duplicates knowledge that already lives in `src/config.rs` and `src/syscall/`, and the duplication causes drift whenever `FileConfig` gains a field or the unit template needs to change.

## Decision

Split the installer along the build/provisioning seam:

- `install.sh` keeps preflight, distro package install, rustup, node/pnpm, and `make install`. Roughly 200 lines.
- All provisioning lives in `denia <subcommand>` (clap-driven): `setup`, `uninstall`, `status`, `doctor`, `rotate-token`. The daemon's run path (`denia` with no subcommand) is unchanged.
- A root `Makefile` is the single build entry point. `install.sh`, CI, and contributors all call `make build` / `make install`.
- Downloaded installer helper scripts use fresh `mktemp` paths, not predictable
  names under `/tmp`.
- Provisioning refuses symlinked config/data path components before applying
  chmod/chown, and secret/config writes use same-directory random temp files
  before atomic persist.
- `install.sh` installs POSIX ACL tooling (`setfacl`), and `denia setup`
  uses it to grant the `denia` system user execute-only traversal through the
  operator home/config parents while keeping the operator home private.

The systemd unit content + TOML config schema are emitted from Rust templates embedded via `include_str!`, so changes track the binary version. `denia setup` is idempotent: re-run keeps keys + config, refreshes the unit.
The generated management listener binds `127.0.0.1:7180` by default; operators
must opt into wider exposure only behind a private network, tunnel, or a future
control-plane TLS serving path.

## Consequences

- Easier: a new `FileConfig` field is added in one place (`src/config.rs`) and reflected by the rendered config; `denia doctor` flags drift.
- Easier: operators get `denia --help`, `denia setup --help`, etc.
- Easier: token rotation is `sudo denia rotate-token` rather than a bespoke shell pipeline.
- Harder: binary grows by ~200 KB (clap + age). Acceptable.
- Harder: bootstrap UX is now two commands (`sudo ./install.sh` then `sudo denia setup`) rather than one. Operator gets a "next step" hint at the end of install.sh.

## Alternatives Considered

- **Keep install.sh as-is.** Rejected: drift cost compounds as the project grows; provisioning logic is not bash's strong suit.
- **`install.sh` execs `denia setup` at the end.** Rejected: the two phases produce distinct logs and have distinct failure modes; chaining them hides the second phase's output behind the first.
- **`cargo xtask` instead of `Makefile`.** Rejected: adds an extra workspace member; `make` is universally available and Makefile recipes are smaller than xtask Rust glue for the three-step build.

## References

- [Spec: 2026-05-28-denia-binary-subcommands-design.md](../superpowers/specs/2026-05-28-denia-binary-subcommands-design.md)
- ADR-023 (TOML config file)
