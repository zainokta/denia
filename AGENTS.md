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
- Privileged runtime tests: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
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

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **denia** (318 symbols, 524 relationships, 15 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

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
