# AGENTS.md

Guidelines for AI agents working on Denia, a Rust backend PaaS that runs workloads with Denia-owned Linux runtime isolation instead of Docker.

## Priorities

- Check `docs/adr/README.md` and existing ADRs before changing architecture.
- Create or update an ADR for runtime isolation, ingress, secrets, persistence, API, or dependency changes.
- Follow system and developer instructions over this file.
- Prefer established local patterns over invention.
- Keep frontend work out of scope unless the user explicitly asks for it.

## Core Workflow

- Keep prompts lean; pull detail from ADRs, docs, and nearby code.
- Use Rust modules with narrow boundaries: API, state, secrets, artifacts, runtime, ingress, and metrics.
- Do not introduce Docker as the service runtime. Dockerfile compatibility belongs in the build path, not the execution path.
- Do not introduce containerd, runc, or a hosted registry unless an ADR accepts the change.
- Keep the single-node control plane simple until a multi-node ADR exists.

## Verification

- Baseline build: `cargo build`
- Tests: `cargo test`
- Format: `cargo fmt --all`
- Lints when available: `cargo clippy --all-targets --all-features`
- Privileged runtime tests must be opt-in and clearly gated because they require root, namespaces, mounts, and cgroup changes.

## Rust Conventions

- Use Rust 2024 edition.
- Use `axum` for HTTP APIs.
- Use explicit domain types for service config, credentials, artifacts, deployments, routes, and metrics.
- Return typed errors at boundaries; avoid panics for expected failures.
- Keep async I/O in API/runtime boundaries and isolate blocking filesystem or SQLite work.
- Add tests before behavior changes.

## Project Conventions

- Management APIs are versioned under `/v1` and protected by a bearer admin token.
- SQLite stores control-plane state; secrets are referenced, not stored raw.
- SOPS-encrypted files hold SSH deploy keys, registry credentials, and service secrets.
- The default SOPS key backend is a host-local age identity with root-only permissions.
- Traefik integration uses the file provider. Denia generates route config and owns loopback bridge listeners.
- Workload ingress is modeled as Denia-owned Unix sockets, with loopback bridge ports only for Traefik compatibility.
- Runtime metrics come from cgroup v2 and procfs.

## Commits And Security

- Commit format: `<type>(<scope>): concise message` where type is `feat`, `fix`, `docs`, `test`, or `refactor`.
- Never commit secrets, local keys, tokens, or generated private config.
- Never log passwords, tokens, SSH private keys, registry credentials, or decrypted SOPS payloads.
- Validate external input and keep host root as the explicit trust boundary.
- Report the exact verification commands run and their results before finishing.

## References

- `docs/adr/README.md`
- `docs/adr/001-initial-backend-architecture.md`
- Rust: <https://www.rust-lang.org/>
- Axum: <https://docs.rs/axum/>
- SOPS: <https://getsops.io/>
- Traefik file provider: <https://doc.traefik.io/traefik/providers/file/>
