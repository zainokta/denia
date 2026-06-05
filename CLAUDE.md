# AGENTS.md

Guidelines for AI agents working on Denia, a Rust backend PaaS that runs workloads with Denia-owned Linux runtime isolation instead of Docker.

## Priorities

- Check `docs/adr/README.md` and existing ADRs before changing architecture.
- Create or update an ADR for runtime isolation, ingress, secrets, persistence, API, or dependency changes.
- Update the landing page (<https://github.com/zainokta/denia-landing>) and documentation (<https://github.com/zainokta/denia-documentation>) repos whenever a new feature, behavior change, or ADR addition lands that they should reflect.
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
- Privileged runtime tests: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
- Privileged runtime tests must be opt-in and clearly gated because they require root, namespaces, mounts, and cgroup changes.

## Rust Conventions

- Use Rust 2024 edition.
- Use `axum` for HTTP APIs.
- Use explicit domain types for service config, credentials, artifacts, deployments, routes, and metrics.
- Return typed errors at boundaries; avoid panics for expected failures.
- Keep async I/O in API/runtime boundaries and isolate blocking filesystem or SQLite work.
- Add tests before behavior changes.
- All UUIDs MUST be UUIDv7. Generate with `uuid::Uuid::now_v7()`. Never call `Uuid::new_v4`, `Uuid::new_v1`, or any non-v7 constructor for IDs that are persisted, returned over the API, or used as keys. In `Cargo.toml`, the `uuid` crate must enable only the `v7` (plus `serde`) features — do not add `v4`/`v1`/`v3`/`v5`. Reason: time-ordered IDs preserve SQLite B-tree index locality and give deterministic ordering for deployments, artifacts, and events.

## Project Conventions

- Management APIs are versioned under `/v1` and protected by a bearer admin token.
- SQLite stores control-plane state; secrets are referenced, not stored raw.
- SOPS-encrypted files hold SSH deploy keys and service secrets. Registry credentials are encrypted by the control plane on the registry CRUD path (no operator-managed `secret_ref`); see ADR-021. The encryption recipient comes from `DENIA_AGE_RECIPIENT`, or is auto-derived from `DENIA_AGE_KEY_FILE` (default `~/.config/denia/age.key`) by parsing the `# public key:` comment `age-keygen` writes. `SOPS_AGE_KEY_FILE` (decryption) still applies at deploy time and may point at the same file.
- The default SOPS key backend is a host-local age identity owned by the installing operator with `denia` group read (`0640 <operator>:denia`), under `~/.config/denia/age.key`. The daemon (running as the `denia` system user) reads it via group bits; the operator backs it up without sudo. See ADR-023.
- Denia is its own L7 ingress: an in-process Pingora (0.8, boringssl) proxy binds `:80`/`:443` on a dedicated thread; no external Traefik, no loopback bridge. The operator must not run a separate proxy (Denia owns `:80`/`:443`). See ADR-020 (supersedes ADR-016).
- Workload ingress upstreams are Denia-owned Unix sockets, dialed directly via `HttpPeer::new_uds` — there are no loopback bridge ports or `bridge_port`.
- TLS is in-process: ACME via `instant-acme` (HTTP-01), per-SNI certs served by a `TlsAccept` callback from an `ArcSwap<CertStore>`; certs persisted `0600` under `<tls_dir>` (`DENIA_TLS_DIR`), boot-loaded before `:443` accepts; renewal scan task. `DENIA_ACME_DIRECTORY_URL` defaults to Let's Encrypt prod — set the LE staging URL for non-prod. ACME is gated to `tls_enabled` services with verified domains. Every hostname is run through `validate_domain` before becoming a route/SNI/ACME identifier.
- Runtime metrics come from cgroup v2 and procfs.
- Runtime security hardening uses `DENIA_USERNS_BASE` (default `100000`) and `DENIA_USERNS_SIZE` (default `65536`). `no_new_privs` and capability-bounding-set drop are applied in-process via the `rustix` syscall module (`src/syscall/`); the `setpriv` host binary is no longer required.
- Overlay capability invariant (do not regress): the daemon's systemd unit MUST grant `CAP_DAC_OVERRIDE` in both `AmbientCapabilities` and `CapabilityBoundingSet` (`src/templates/denia.service.in`). The per-replica overlay is mounted pre-userns by the non-root `denia` daemon (ADR-026); `upper`/`work` are chowned to `DENIA_USERNS_BASE` (via `CAP_CHOWN`) so the workload owns its writable layer — which the daemon does not own. overlayfs DAC-checks its `work/work` + copy-up setup against the mounter's fsuid, so without `CAP_DAC_OVERRIDE` it SILENTLY mounts the merged layer **read-only** and every workload write fails **EROFS** (boot autostart + autoscale both fail the `/.denia-rw-probe` in `src/syscall/ns.rs`). Do NOT "fix" overlay mount errors by flipping `upper`/`work` to daemon ownership — that makes the mount fail **EACCES** and also breaks in-userns file ownership (base-uid files map to root inside the userns; daemon-owned files map to `nobody`). If you change overlay mount location/ownership/caps, re-verify a workload can actually write its root. See ADR-026.
- The web console (`web/`) is served by the binary itself: `src/web.rs` embeds the SPA build (`web/dist/client`) via `rust-embed` and `build_router` adds it as a fallback after `/healthz` and `/v1`. SSR is dropped; the UI is a static SPA on the same origin as `/v1`. See ADR-004. Build flow: `cd web && pnpm build`, then `cargo run` serves API + UI on `DENIA_BIND_ADDR` (default `127.0.0.1:7180`). A release `cargo build` requires `web/dist/client` to exist first (it is gitignored). Production installs are `sudo ./install.sh` (build + binary) then `sudo denia setup` (provisioning); see ADR-025. Upgrades use `sudo denia update`, which downloads the latest prebuilt binary from the GitHub release, verifies it against a pinned minisign signature over `SHA256SUMS`, atomically swaps `/usr/local/bin/denia`, and restarts the service; releases are built by `.github/workflows/release.yml`. See ADR-029.

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

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **denia** (6596 symbols, 15773 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## When Debugging

1. `gitnexus_query({query: "<error or symptom>"})` — find execution flows related to the issue
2. `gitnexus_context({name: "<suspect function>"})` — see all callers, callees, and process participation
3. `READ gitnexus://repo/denia/process/{processName}` — trace the full execution flow step by step
4. For regressions: `gitnexus_detect_changes({scope: "compare", base_ref: "main"})` — see what your branch changed

## When Refactoring

- **Renaming**: MUST use `gitnexus_rename({symbol_name: "old", new_name: "new", dry_run: true})` first. Review the preview — graph edits are safe, text_search edits need manual review. Then run with `dry_run: false`.
- **Extracting/Splitting**: MUST run `gitnexus_context({name: "target"})` to see all incoming/outgoing refs, then `gitnexus_impact({target: "target", direction: "upstream"})` to find all external callers before moving code.
- After any refactor: run `gitnexus_detect_changes({scope: "all"})` to verify only expected files changed.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Tools Quick Reference

| Tool | When to use | Command |
|------|-------------|---------|
| `query` | Find code by concept | `gitnexus_query({query: "auth validation"})` |
| `context` | 360-degree view of one symbol | `gitnexus_context({name: "validateUser"})` |
| `impact` | Blast radius before editing | `gitnexus_impact({target: "X", direction: "upstream"})` |
| `detect_changes` | Pre-commit scope check | `gitnexus_detect_changes({scope: "staged"})` |
| `rename` | Safe multi-file rename | `gitnexus_rename({symbol_name: "old", new_name: "new", dry_run: true})` |
| `cypher` | Custom graph queries | `gitnexus_cypher({query: "MATCH ..."})` |

## Impact Risk Levels

| Depth | Meaning | Action |
|-------|---------|--------|
| d=1 | WILL BREAK — direct callers/importers | MUST update these |
| d=2 | LIKELY AFFECTED — indirect deps | Should test |
| d=3 | MAY NEED TESTING — transitive | Test if critical path |

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/denia/context` | Codebase overview, check index freshness |
| `gitnexus://repo/denia/clusters` | All functional areas |
| `gitnexus://repo/denia/processes` | All execution flows |
| `gitnexus://repo/denia/process/{name}` | Step-by-step execution trace |

## Self-Check Before Finishing

Before completing any code modification task, verify:
1. `gitnexus_impact` was run for all modified symbols
2. No HIGH/CRITICAL risk warnings were ignored
3. `gitnexus_detect_changes()` confirms changes match expected scope
4. All d=1 (WILL BREAK) dependents were updated

## Keeping the Index Fresh

After committing code changes, the GitNexus index becomes stale. Re-run analyze to update it:

```bash
npx gitnexus analyze
```

If the index previously included embeddings, preserve them by adding `--embeddings`:

```bash
npx gitnexus analyze --embeddings
```

To check whether embeddings exist, inspect `.gitnexus/meta.json` — the `stats.embeddings` field shows the count (0 means no embeddings). **Running analyze without `--embeddings` will delete any previously generated embeddings.**

> Claude Code users: A PostToolUse hook handles this automatically after `git commit` and `git merge`.

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
