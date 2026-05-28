# TODO

11. install.sh + denia subcommands -> DONE (ADR-025). `install.sh` builds the binary and copies it to /usr/local/bin/denia (preflight + OS deps + rustup + node + `make install`). Provisioning (system user, /var/lib/denia layout, ~/.config/denia/{config.toml,admin.token,age.key}, systemd unit, service start) lives in `denia setup`. Other subcommands: `denia uninstall [--purge]`, `denia status`, `denia doctor`, `denia rotate-token`. See docs/superpowers/specs/2026-05-28-denia-binary-subcommands-design.md + docs/superpowers/plans/2026-05-28-denia-binary-subcommands.md.
