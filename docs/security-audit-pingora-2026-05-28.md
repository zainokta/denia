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

---

## Chunk B — Phase 3+4 (DeniaProxy request path + in-process ACME/TLS)

Commits `514a0e9` (proxy) + `74edc16` (ACME/TLS). Additive: no Traefik/bridge or `main.rs`/`app.rs` cutover (Chunk C).

**Secrets discipline — PASS.** No `Debug`/`Serialize`/`Clone`-to-log on any private-key holder: `ChallengeStore` and `IssuedCert` omit those derives; `instant_acme::KeyAuthorization`'s own `Debug` is redacted; the `:80` `request_filter`/`logging()` path logs no request headers and runs every access-log path through `sanitize_path` (UUID/token redaction). Account key + leaf key files are written **atomically** (temp file created at mode `0600` up front, then `rename`) so they are never world-readable mid-write — verified by `persist_cert_writes_files_at_mode_0600` / `_is_atomic_no_temp_left_behind` / `account_key_persisted_at_mode_0600`.

**A1 carry-forward — HONORED.** Every externally-influenced hostname is run through `validate_domain` before it becomes an ACME order identifier (`AcmeDriver::issue`), a persisted cert directory name (`persist_cert` — also blocks `../` path traversal, test `persist_cert_rejects_path_traversal_domain`), or an SNI selection key (`CertStore::try_insert`, `load_certs_from_disk` skips non-domain dirs).

**TLS decline — PASS (Spike 0.3 honored).** `DeniaCertResolver::certificate_callback` installs no cert for an unknown / absent SNI → clean `TLSHandshakeFailure`, never a default/wrong cert. Decision isolated in the pure `resolve_sni_cert` (5 unit tests incl. case-insensitivity + empty-store decline).

| # | Sev | Finding | Status |
|---|-----|---------|--------|
| A7 | MINOR | No zeroization of private-key bytes on drop. Account key (`instant_acme::Key`), leaf keys (`pingora::tls::pkey::PKey<Private>` inside `ArcSwap<CertStore>`), and the transient `IssuedCert.key_pem`/account PKCS#8 DER (`PrivatePkcs8KeyDer`) are all foreign types whose secret bytes live in boring/ring-owned allocations. | ⏸️ **deferred (documented trade-off)** |

**A7 assessment:** zeroize-on-drop is **not cheap** here — the secret bytes are owned by foreign types (`boring::pkey::PKey`, `ring`/`aws-lc-rs`-backed `instant_acme::Key`, `rustls_pki_types::PrivatePkcs8KeyDer`). Wrapping them in `Zeroizing` only protects copies we make, not the foreign allocations, and we cannot add `Drop` to foreign types. The realistic exposures are: (a) the in-memory `ArcSwap<CertStore>` (resident for the process lifetime by design — selection must be sync), and (b) the transient `IssuedCert`/account-DER buffers during issuance. The mitigation already in place is no-log + no-derive discipline and `0600` at-rest files. Given foreign ownership, the residual risk is a memory-disclosure / core-dump scenario, which is out of scope for the single-node trust boundary (host root is already trusted). **Decision: accept and document; revisit only if a `Zeroizing` newtype around our own copies becomes warranted.** No code change this chunk.

### Chunk B review (infra GREEN / security AMBER)

| # | Sev | Finding | Status |
|---|-----|---------|--------|
| B1 | BLOCKER | `/.well-known/acme-challenge/{token}` (and `denia-challenge`) mounted behind `rate_limit_login` (`LoginRateLimiter`, default 5 req/60s per IP). Let's Encrypt validates from multiple vantage points + retries → `429` silently breaks issuance/renewal once `:80` is public. Spec lists rate limiting as out of scope. | ✅ fix before Chunk C |
| B2 | MAJOR | (= A3) Unauthenticated client on public `:80`/`:443` can trigger `ActivationHook` for any cold *routed* service; bounded by single-flight + `ACTIVATION_WAIT` (not unbounded), but can keep cold services hot. | ⏸️ Chunk C: add abuse limit OR accept explicitly in ADR-020 |
| B3 | MINOR | `<tls_dir>`/`<domain>` dirs created with default umask (~0755); leaf files are 0600 so keys aren't exposed, but 0700 dirs = defense-in-depth. | ✅ fixed (`211acfc`) |

**Resolution (commit `211acfc`):** B1 ✅ — both `/.well-known/{acme,denia}-challenge/{token}` routes moved off the login rate limiter onto the unauthenticated branch (siblings of `/healthz`, before `/v1`); login/admin routes keep their limiters. Test `challenge_routes_are_not_login_rate_limited` fires 20 rapid requests, asserts no 429. B3 ✅ — `create_private_dir_all` (`DirBuilder.mode(0o700)`) for tls dirs. 342 tests pass. **B2/A3 (unauthenticated activation trigger) deferred to Chunk C decision → document in ADR-020 (bounded by single-flight + `ACTIVATION_WAIT`; rate-limiting is out of scope per spec).**

**PASS (security-verified, not just self-reported):** atomic 0600 secret writes (temp@0600→rename, correct syscall order); `validate_domain` enforced on all three sinks (ACME identifier, cert dir name w/ `../` blocked, SNI key); fail-closed TLS decline on unknown/absent SNI; no secret bytes in `Debug`/`Serialize`/errors/logs; `sanitize_path` redacts UUID/token segments. A7 (no zeroization) deferral independently confirmed sound.

**Carry-forward for Chunk C:** `main` must (1) build a single shared `ChallengeStore`, clone it into both the `AcmeDriver` and `AppState.acme_challenges` so the axum handler and issuer see the same map; (2) **boot-load certs** via `load_certs_from_disk(tls_dir)` + `IngressState::swap_certs` **before** `:443` accepts; (3) call `AcmeDriver::new(tls_dir, acme_directory_url, acme_email, challenges)` and spawn issuance + a renewal scan (`select_renewals(&certs, RENEWAL_WINDOW_DAYS)`); (4) `build_server(Arc<IngressState>, &IngressServerConfig)` now returns `Result<Server, ServerBuildError>` and binds both `:80` and `:443`. A3 (unauthenticated cold-start trigger) and A6 (`Server::new(None)` still `Result`-but-callers-may-`expect`) remain ⏸️ for Chunk C.

---

## Chunk C — Phase 5 (big-bang cutover) review (infra RED / security GREEN)

Commits `a1e6853..6ed7e45`.

| # | Sev | Finding | Status |
|---|-----|---------|--------|
| C1 | BLOCKER | **Data plane broken.** `proxy.rs::upstream_peer` resolves `Host` → `route.service_name` (human name) and passes it to `resolve_or_activate`, but replica pools are keyed by `service.id.to_string()` (coordinator/lifecycle) and the activator parses the key as a UUID. Every request misses the pool → activator fails → 503. No deployed service reachable. | ✅ fix |
| C2 | MAJOR | Rewritten `coordinator_registers_route_and_replica_on_promotion` asserts route + pool separately but never exercises the `resolve(host)→resolve_or_activate` join, so it structurally cannot catch C1. | ✅ add end-to-end test |
| C3 | MINOR | `controller.rs`/`lifecycle.rs` field/param still named `bridge: Arc<IngressState>` — stale; rename to `ingress`. | ✅ fix |
| C4 | MINOR | `lifecycle.rs` tests assert `healthy_count(&service_name)` (wrong key) — trivially passes, masks regressions. Assert on `service_id.to_string()`. | ✅ fix |
| C5 | MINOR | `DENIA_ACME_DIRECTORY_URL` defaults to LE **production** — document the staging override to avoid rate-limit burns in non-prod. | ⏸️ Chunk E docs |

**Resolution (commit `10b94ad`):** C1 ✅ — `RouteSpec` gained `service_id: String` (`#[serde(skip)]`, set to `service.id.to_string()` in coordinator + routes); `proxy.rs::upstream_peer` now keys `resolve_or_activate` by `route.service_id` (pool key), `service_name` kept for access log only. C2 ✅ — end-to-end join test in `state.rs` (resolve(host)→pool key→resolve_or_activate returns socket; name-keyed lookup returns `None`); confirmed red pre-fix, green post-fix. C3 ✅ — `bridge`→`ingress` rename. C4 ✅ — lifecycle tests assert on `service_id`. 319 tests pass, clippy clean. C5 (LE-staging docs) ⏸️ Chunk E.

**Security PASS (verified):** proxy binds only `0.0.0.0:80`/`:443`; control plane stays on `bind_addr` (loopback) — not newly exposed. Certs boot-loaded before `:443` accepts (no empty-store window). ACME issuance gated to `tls_enabled` + `list_verified_hostnames` only; `validate_domain` re-applied. Challenge hop dials the fixed `control_backend` SocketAddr (host/path never influence upstream) → no SSRF/open-proxy. Bind failure logs + control plane survives; no insecure fallback (no API-on-public-port, no TLS-disable). `/v1/ingress/config` + Traefik files removed cleanly; no dead RBAC entry.
