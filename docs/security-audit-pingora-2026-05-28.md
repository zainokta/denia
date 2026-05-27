# Security Audit — Pingora Ingress Migration

**Started:** 2026-05-28
**Scope:** Replacement of managed-Traefik + loopback bridge with in-process Pingora 0.8 (boringssl) ingress, in-process ACME (instant-acme, HTTP-01), direct UDS upstreams.
**Reviewers:** per-chunk senior-infra + senior-security subagent pair.
**Plan:** `docs/superpowers/plans/2026-05-28-pingora-ingress.md` · **Spec:** `docs/superpowers/specs/2026-05-28-pingora-ingress-design.md`

Severity: **BLOCKER** (must fix before dependent chunk) · **MAJOR** (fix this migration) · **MINOR** (track).
Status: ❌ open · ✅ fixed · ⏸️ deferred (with target chunk).

---

## Chunk A — Phase 1+2 (deps, skeleton, IngressState + bridge brain)

Diff: `24b59a4^..a687c34`. Both reviewers: **AMBER**.

| # | Sev | Finding | Status |
|---|-----|---------|--------|
| A1 | BLOCKER | `RouteTable::upsert`/`RouteSpec` perform **no domain validation**; `IngressError::{EmptyServiceName,MissingDomain,InvalidDomain}` declared but never constructed. Domain strings flow unsanitized into routing keys, **ACME order identifiers**, and **TLS SNI** (Chunk B) → cert mis-issuance / LE rate-limit risk. Old `traefik.rs` rejected empty/backtick/CR/LF. | ✅ fix in Chunk A follow-up |
| A2 | MAJOR | Case-sensitivity: `BTreeMap::get` exact-match means mixed-case `Host` headers silently 404. Normalize domains to lowercase ASCII (punycode) at ingest. | ✅ fix in Chunk A follow-up |
| A3 | MAJOR | Unbounded maps: `by_host`, `by_sni`, `pools`, `activation_gates`; `activation_gate` grows a permanent per-service entry, never pruned. Operator-fed now (bounded), but public `:80` `resolve_or_activate` lets an unauthenticated client trigger `ActivationHook` for any cold routed service with no cross-service rate limit. | ⏸️ revisit when proxy wired (Chunk B/C) |
| A4 | MINOR | `ACTIVATION_WAIT` unused; `resolve_or_activate` has no overall timeout — only 5×20ms post-activation retries. If `activator.activate()` hangs, the proxy hot path blocks indefinitely. | ✅ wrap call in `timeout(ACTIVATION_WAIT, …)` (Chunk A follow-up) |
| A5 | MINOR | `CertStore::insert` accepts arbitrary SNI string, no validation (same root as A1). | ✅ fold into A1 validation |
| A6 | MINOR | `build_server`/`Server::new(None).expect()` panics; prefer `Result` per CLAUDE.md "no panics for expected failures". | ⏸️ before cutover (Chunk C) |
| A7 | MINOR | No zeroization of `PKey<Private>` on drop; account + leaf keys resident in `ArcSwap<CertStore>`. | ⏸️ assess in ACME chunk (Chunk B) |
| A8 | MINOR | `swap_routes`/`swap_certs` are whole-table last-writer-wins; safe only under a single control-plane writer. Document the invariant before multi-writer. | ⏸️ document in Chunk C |
| A9 | MINOR | `cargo audit` not installed/run; advisories unverified for pingora/boring/arc-swap. | ⏸️ add to verification (Chunk E) |
| A10 | MINOR | Test gaps vs old bridge: no concurrent single-flight activation test (gate concurrency unexercised), no `remove_replica` cursor-bounds test, no `set_last_activity` round-trip. | ✅ add concurrent single-flight test (Chunk A follow-up) |

**Key handling — PASS:** `ParsedCert` omits `Debug`/`Serialize`; `IngressState` has no `Debug`; no `tracing` touches key bytes.

**Resolution (commit `f2766e8`):** A1, A2, A4, A5, A10 ✅ fixed — `validate_domain` (rejects empty/whitespace/control/backtick/CRLF/wildcard/non-ASCII/overlong/dot-edges, returns lowercased), `RouteTable::try_upsert` + `CertStore::try_insert`, lowercase lookups in `resolve`/`get`, `resolve_or_activate` wrapped in `timeout(ACTIVATION_WAIT)` → `ActivationError::Timeout`, concurrent single-flight test (16 racers → 1 activation). 309 tests pass. **Carry-forward for Chunk C:** callers (coordinator/routes) MUST use `try_upsert` (not the infallible `upsert`, which silently skips invalid domains) and surface `InvalidDomain` at the API boundary. A3/A6/A7/A8/A9 remain ⏸️ per target chunk.
