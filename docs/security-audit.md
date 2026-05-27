# Denia PaaS — Security Audit Report

**Date**: 2026-05-27
**Auditors**: 5 parallel specialist security agents (Auth/API, Dependencies/CVE, Runtime/Syscall, OCI/Data, Docs/Threat-Model)
**Scope**: Full repository — source code, configuration, dependencies, CI/CD, documentation, ADRs

---

## Executive Summary

Denia demonstrates **strong security fundamentals**: Argon2id password hashing, constant-time token comparison, SHA-256 token hashing, RBAC with project-scoped roles, comprehensive security headers, SOPS-based secrets management, parameterized SQL throughout (zero injection vectors), layered path traversal defenses, SUID/SGID stripping, and decompression bomb limits.

However, the audit identified **4 Critical**, **9 High**, **20 Medium**, and **16 Low/Informational** findings. The most urgent issues are:

1. **Wrong x86_64 seccomp syscall numbers** — `mount`/`umount2` are NOT actually blocked
2. **Services launch without seccomp filter** — the primary deployment mode bypasses syscall filtering
3. **No digest verification on registry blob pulls** — supply chain attack vector
4. **No audit logging** — complete repudiation gap for all control-plane actions

---

## Scope

| Area | Files Reviewed |
|------|---------------|
| Application code | All `src/**/*.rs` (95+ files) |
| Authentication & authorization | `src/auth/`, `src/api/auth.rs`, `src/api/users.rs`, `src/api/tokens.rs` |
| API endpoints | All `src/api/*.rs` (16 files) |
| Runtime isolation | `src/runtime/`, `src/syscall/`, `src/workload_launcher.rs` |
| OCI image handling | `src/oci/`, `src/artifacts/` |
| Data layer | `src/repo/sqlite/` (12 files), `src/domain/` (10 files) |
| Ingress | `src/ingress/` (4 files) |
| Secrets | `src/secrets.rs`, `src/config.rs` |
| Dependencies | `Cargo.toml`, `Cargo.lock`, `web/package.json`, `web/pnpm-lock.yaml` |
| Documentation | All ADRs (001–015), design specs, README, AGENTS.md, PRODUCT.md, DESIGN.md, TODO.md |
| Configuration | `.gitignore`, `.serena/` |
| Tests | `tests/` (4 files) |

## Methodology

- **Code security review**: Manual source code analysis of all Rust modules
- **Dependency/CVE analysis**: Version cross-referencing against known CVE databases
- **Runtime isolation review**: Deep analysis of namespace, seccomp, capability, and cgroup code
- **OCI/data layer review**: Supply chain, SQL, and persistence security analysis
- **Documentation/threat model review**: STRIDE analysis, ADR gap analysis, architecture security review
- Findings are evidence-based; CVE claims marked "requires verification" where live confirmation was not possible

---

## Repository Overview

| Attribute | Value |
|-----------|-------|
| Language | Rust (2024 edition) |
| HTTP framework | axum 0.8 |
| Database | SQLite (bundled via rusqlite 0.39) |
| TLS | rustls + aws-lc-rs |
| Runtime isolation | Linux namespaces (user/pid/mount/net/uts/ipc) + cgroup v2 + seccomp denylist |
| Secrets | SOPS with age encryption |
| Ingress | Traefik file provider → loopback bridge → Unix socket → workload |
| Architecture | Single-node monolith (control plane + node agent + embedded SPA) |

---

## Threat Model

### Trust Boundaries

```
INTERNET (untrusted)
    │ :80/:443
    ▼
┌──────────────────────┐
│ Traefik (TLS term)   │  TB1: Network edge
└──────────┬───────────┘
           │ 127.0.0.1:bridge_port
┌──────────▼───────────┐
│ Loopback Bridge      │  TB2: Network → Process
│ (tee_proxy + access  │
│  log capture)        │
└──────────┬───────────┘
           │ Unix socket
┌──────────▼───────────┐
│ Workload Namespace   │  TB3: Process → Sandbox
│ (user/pid/mount/uts/ │     (user-ns, seccomp, caps,
│  ipc/net namespaces) │      cgroup v2)
└──────────────────────┘

┌──────────────────────┐
│ Control Plane API    │  TB4: User → API
│ (/v1 — bearer auth,  │     (auth, RBAC, rate limit)
│  RBAC, rate limit)   │
└──────────┬───────────┘
           │
    ┌──────┼──────┐
    ▼      ▼      ▼
 SQLite  SOPS   Traefik
 (state) (secrets) (dynamic.yml)
 TB5     TB6     TB7
```

### Threat Actors

| Actor | Motivation | Primary Targets |
|-------|-----------|----------------|
| External attacker | Data theft, service disruption | Traefik, workload services, ACME challenges |
| Malicious workload | Container escape, host compromise | Namespace boundary, seccomp filter, socket proxy |
| Compromised operator | Cross-project access, data exfiltration | API, workload logs, domain verification |
| Supply chain attacker | Backdoor via OCI image | Registry, image layers, build pipeline |
| Local attacker | Privilege escalation on host | SQLite file, SOPS files, age key, Unix sockets |

### STRIDE Summary

| Threat | Highest Risk Component | Current Mitigation | Key Gap |
|--------|----------------------|-------------------|---------|
| **Spoofing** | Auth endpoint | Bearer tokens, argon2, rate limiting | No MFA; bootstrap token never expires |
| **Tampering** | Traefik config | Denia-exclusive writes | No file integrity monitoring |
| **Repudiation** | All user actions | **None** | **No audit log exists** |
| **Info Disclosure** | Workload logs | Operator-gated | No log scrubbing for secrets |
| **DoS** | Rate limiter | In-memory rate limiting | Behind Traefik, all IPs = 127.0.0.1 |
| **Elevation** | Namespace escape | User-ns, caps, seccomp | Shared UID range; seccomp denylist (not allowlist) |

---

## Findings by Severity

### Critical (4)

#### C-1: Wrong x86_64 Seccomp Syscall Numbers — mount/umount2 NOT Blocked

| Field | Value |
|-------|-------|
| **File** | `src/syscall/seccomp.rs:95-121` |
| **Description** | The x86_64 seccomp denylist contains incorrect syscall numbers. `mount` is listed as 177 (actual: 165), `umount2` as 178 (actual: 166), `sethostname` as 139 (actual: 170), `swapon` as 180 (actual: 167). The filter blocks the wrong syscalls — `mount` and `umount2` are effectively unblocked. |
| **Exploit** | A workload with `CAP_SYS_ADMIN` inside its user namespace can call `mount()` to mount a new `/proc`, unmask sensitive paths, or rearrange the filesystem via bind mounts. |
| **Impact** | Defense-in-depth failure. Kernel vulnerability in mount subsystem could enable container escape. |
| **Fix** | Use `libc::SYS_mount as u32` etc. instead of raw numbers. The aarch64 denylist is correct. |
| **Priority** | **P1** |

#### C-2: Services Launch Without Seccomp Filter

| Field | Value |
|-------|-------|
| **File** | `src/syscall/ns.rs:189-194`, `src/ingress/socket_proxy.rs:121-122` |
| **Description** | `LinuxRuntime::plan()` calls `.with_deferred_hardening()` for services, setting `seccomp = false`. The socket proxy re-applies `no_new_privs` and `drop_bounding_caps` but **never installs the seccomp filter**. All long-running services operate without syscall filtering. |
| **Exploit** | A malicious service workload can call `bpf`, `ptrace`, `io_uring_setup`, `unshare`, `setns`, and all other syscalls the filter was designed to block. |
| **Impact** | Complete bypass of seccomp for the primary deployment mode. Jobs (one-shot) correctly apply seccomp, but services do not. |
| **Fix** | Add `crate::syscall::seccomp::install_filter()?;` to `socket_proxy.rs::run()` after capability dropping. |
| **Priority** | **P1** |

#### C-3: No Digest Verification on Registry Blob Pulls

| Field | Value |
|-------|-------|
| **File** | `src/oci/registry.rs:56-59` |
| **Description** | `pull_blob` writes blob data directly to file without verifying the content digest matches the manifest descriptor. The OCI layout path (`src/oci/layout.rs:71,89-108`) correctly verifies digests, but the registry pull path has no equivalent check. |
| **Exploit** | A compromised registry serves tampered layer content. The manifest declares `sha256:abc...` but the blob contains attacker code. The system unpacks and runs it without detection. |
| **Impact** | Supply chain attack — arbitrary code execution inside deployed workloads. |
| **Fix** | After `pull_blob`, compute SHA-256 of the written file and compare against `desc.digest`. Reject on mismatch. |
| **Priority** | **P1** |

#### C-4: No Audit Logging — Complete Repudiation Gap

| Field | Value |
|-------|-------|
| **Source** | ADR-008 (line 117: "audit log (later)"), all ADRs |
| **Description** | No audit logging system exists. There is no record of who deployed, deleted, or modified anything. Bootstrap super-admin token actions are completely untraceable. |
| **Exploit** | A compromised operator account operates undetected. Post-incident forensics are impossible. |
| **Impact** | Cannot meet SOC 2, PCI-DSS, or HIPAA requirements. Complete inability to investigate security incidents. |
| **Fix** | Author ADR-016 for immutable audit log: append-only table capturing `(timestamp, principal, action, resource_type, resource_id, outcome, source_ip)` for all mutating API operations. |
| **Priority** | **P1** |

---

### High (9)

#### H-1: User Enumeration via Timing Side-Channel in Login

| Field | Value |
|-------|-------|
| **File** | `src/repo/sqlite/users.rs:122-150` |
| **Description** | `verify_login_q` queries for a user by username, then conditionally calls Argon2id verification (~100-300ms) only if the user exists. Non-existent users return instantly (~1ms). The timing difference is easily measurable. |
| **Fix** | Always perform a dummy Argon2id verification against a constant hash when the user is not found. |
| **Priority** | P1 |

#### H-2: ServiceConfig ID and Validation Bypass via Direct Deserialization

| Field | Value |
|-------|-------|
| **File** | `src/api/services.rs:60-79` |
| **Description** | `put_service` accepts `Json<ServiceConfig>` — the full domain object including `id` — directly from the request body. Direct deserialization bypasses all constructor validation (name non-emptiness, domain format, port > 0, health check). |
| **Fix** | Accept a `PutServiceRequest` DTO (without `id`) and construct via `ServiceConfig::new()`. |
| **Priority** | P1 |

#### H-3: `User` Debug Output Exposes Password Hashes

| Field | Value |
|-------|-------|
| **File** | `src/domain/user.rs:17` (`#[derive(Debug)]`) |
| **Description** | `User` derives `Debug`, which includes `password_hash` in output. Any `log::debug!`, `log::error!`, or `{:?}` formatting will include the hash. `#[serde(skip_serializing)]` only prevents JSON serialization, not Debug output. |
| **Fix** | Implement custom `Debug` for `User` that redacts `password_hash` as `"[REDACTED]"`. |
| **Priority** | P1 |

#### H-4: SQLite Database File Permissions Not Explicitly Restricted

| Field | Value |
|-------|-------|
| **File** | `src/repo/sqlite/pool.rs:36-42` |
| **Description** | `Connection::open(path)` creates the SQLite file with the process's default umask. On a typical system with umask `022`, the file would be world-readable (`0644`). The database contains password hashes, session tokens, API token hashes, and all control-plane state. |
| **Fix** | Call `std::fs::set_permissions(path, Permissions::from_mode(0o600))` immediately after opening. |
| **Priority** | P1 |

#### H-5: Seccomp Denylist Missing Critical Syscalls

| Field | Value |
|-------|-------|
| **File** | `src/syscall/seccomp.rs:91-152` |
| **Description** | Even after fixing incorrect numbers, the denylist is missing: `unshare` (272), `setns` (308), `clone3` (435), `io_uring_setup` (425), `io_uring_enter` (426), `io_uring_register` (427), `personality` (135), `keyctl` (250), `userfaultfd` (323), `acct` (163), `syslog` (103). |
| **Fix** | Add missing syscalls. Consider switching to an allowlist approach for stronger security. |
| **Priority** | P2 |

#### H-6: File Descriptor Leak Across fork()

| Field | Value |
|-------|-------|
| **File** | `src/syscall/ns.rs:403-417` |
| **Description** | `spawn_namespaced_process` calls `fork()`, duplicating all parent FDs (SQLite connections, API listener socket, log files) into the child. Non-CLOEXEC FDs survive into the workload process after `execve`. |
| **Fix** | In `child_exec`, before `execve`, close all inherited FDs except stdin/stdout/stderr via `/proc/self/fd` enumeration. |
| **Priority** | P2 |

#### H-7: .gitignore Lacks Secret and Data Protection Patterns

| Field | Value |
|-------|-------|
| **File** | `.gitignore` |
| **Description** | No patterns for `*.sops.yaml`, `*.sqlite3`, `.env`, `age.key`, `secrets/`, `*.pem`, `*.key`, `acme.json`. A developer running Denia with `DENIA_DATA_DIR=./data` could commit secrets, database, and keys to Git. |
| **Fix** | Add defensive patterns for all secret, database, key, and runtime data file types. |
| **Priority** | P1 |

#### H-8: Bootstrap Admin Token Is Permanent, Unrevokable Super-Admin

| Field | Value |
|-------|-------|
| **Source** | ADR-008, `src/auth/middleware.rs` |
| **Description** | `DENIA_ADMIN_TOKEN` grants permanent super-admin access. No mechanism to rotate without restart, revoke after setup, detect usage, or audit actions. |
| **Fix** | Implement the approved admin bootstrap design. Add a `bootstrap_completed` flag that invalidates the bootstrap token. |
| **Priority** | P1 |

#### H-9: Rate Limiter Ineffective Behind Traefik

| Field | Value |
|-------|-------|
| **File** | `src/rate_limit.rs:81-87` |
| **Description** | IP extraction uses `ConnectInfo<SocketAddr>` (TCP peer). Behind Traefik, all requests arrive from `127.0.0.1`, meaning all users share one rate-limit bucket. Login brute-force protection is effectively disabled. |
| **Fix** | Extract client IP from `X-Forwarded-For` with trusted proxy allowlist (loopback). Fall back to per-user rate limiting. |
| **Priority** | P1 |

---

### Medium (20)

| # | Title | File | Priority |
|---|-------|------|----------|
| M-1 | Job object accepted without validation in `create_job` | `src/api/jobs.rs:34-42` | P2 |
| M-2 | Argon2 parameters below OWASP recommendations (t_cost=1) | `src/auth/credentials.rs:11-13` | P2 |
| M-3 | No absolute session timeout — sliding window only | `src/auth/middleware.rs:38` | P2 |
| M-4 | Potential SSRF in domain verification (internal network probing) | `src/api/domains.rs:103-168`, `src/verification/http.rs` | P2 |
| M-5 | Shared UID range across all containers | `src/runtime/linux.rs:79-80` | P2 |
| M-6 | `pids.max` not set by default — fork bomb DoS | `src/runtime/linux.rs:202-211` | P2 |
| M-7 | Traefik YAML injection via `route_key` | `src/ingress/traefik.rs:55-83` | P2 |
| M-8 | Silent error suppression for job resource limits | `src/runtime/linux.rs:412-429` | P2 |
| M-9 | OCI unpacker symlink TOCTOU window | `src/oci/unpack.rs:163-223` | P2 |
| M-10 | Non-transactional user deletion | `src/repo/sqlite/users.rs:89-120` | P2 |
| M-11 | Scheduler full-table scan every second | `src/scheduler.rs:69` | P2 |
| M-12 | Session token hashes exposed via `Session.token` serialization | `src/repo/sqlite/users.rs:323-341` | P2 |
| M-13 | Opaque whiteout deletion doesn't re-validate paths | `src/oci/unpack.rs:70-88` | P2 |
| M-14 | `safe_artifact_name` collision potential | `src/artifacts/acquirer.rs:336-347` | P2 |
| M-15 | Deployment coordinator non-atomic deploy | `src/deploy/coordinator.rs:117-167` | P2 |
| M-16 | No formal threat model document | All ADRs | P2 |
| M-17 | Security headers implemented but undocumented | `src/app.rs:306-329` | P2 |
| M-18 | No documented session security parameters | ADR-008 | P2 |
| M-19 | No domain re-verification after initial check | ADR-013 | P2 |
| M-20 | Autoscaling design lacks security analysis | `docs/superpowers/specs/2026-05-27-autoscaling-design.md` | P2 |

---

### Low (12)

| # | Title | File | Priority |
|---|-------|------|----------|
| L-1 | Redundant `touch_session` in middleware for non-session auth | `src/auth/middleware.rs:38` | P3 |
| L-2 | Missing `Permissions-Policy` header | `src/app.rs:306-335` | P3 |
| L-3 | No username format validation | `src/api/users.rs:37-58` | P3 |
| L-4 | No maximum password length — Argon2 DoS vector | `src/api/users.rs:45` | P3 |
| L-5 | Partial session token prefix exposed in `list_sessions` | `src/api/auth.rs:106-119` | P3 |
| L-6 | Secret path validation silently bypassed when `canonicalize()` fails | `src/secrets.rs:105-116` | P3 |
| L-7 | `/proc` masking incomplete — missing sensitive paths | `src/syscall/ns.rs:917-939` | P3 |
| L-8 | Bridge port allocator wraps without collision detection | `src/ingress/bridge.rs:36-53` | P3 |
| L-9 | No graceful shutdown (SIGTERM) before SIGKILL | `src/runtime/fs_helpers.rs:200-229` | P3 |
| L-10 | Log files created with default umask permissions | `src/runtime/linux.rs:515` | P3 |
| L-11 | `list_users` loads all password hashes into memory unnecessarily | `src/repo/sqlite/users.rs:62-87` | P3 |
| L-12 | Access log is volatile — no forensic persistence | ADR-009 | P3 |

### Informational (4)

| # | Title | File |
|---|-------|------|
| I-1 | `revoke_all_sessions` invalidates the calling session | `src/api/auth.rs:123-131` |
| I-2 | Feature flags are appropriately scoped | `Cargo.toml` |
| I-3 | Cryptographic stack is well-chosen (ChaCha20 CSPRNG, aws-lc-rs, constant-time comparison) | Multiple |
| I-4 | Documentation leaks detailed internal architecture (accepted trade-off for open source) | README, ADRs |

---

## Dependency and CVE Review

### Key Findings

| Dependency | Version | Issue | Severity | Status |
|-----------|---------|-------|----------|--------|
| `tar` | 0.4.46 | CVE-2026-33056 (symlink chmod attack) | High | **Patched** — version > 0.4.45; custom extraction path bypasses vulnerable code |
| SQLite (bundled) | 3.51.3 | CVE-2025-70873 (zipfile OOB read) | High | **Requires verification** — zipfile extension likely not compiled into library builds |
| `h2` | 0.4.14 | CVE-2025-8671 (HTTP/2 stream reset DoS) | Low | **Requires verification** — affected versions still being enumerated |
| `oci-client` | 0.17.0 | Version discrepancy with crates.io | Medium | **Requires verification** — confirm checksum matches published crate |
| `argon2` | 0.5.3 | t_cost=1 below OWASP minimum | Medium | **Confirmed** |
| TanStack packages | `latest` | Supply chain risk from unpinned versions | Medium | **Confirmed** — pin to resolved versions from lockfile |
| `effect` | 4.0.0-beta.70 | Pre-release dependency in production | Medium | **Confirmed** |

### Dependency Version Summary

| Crate | Version | Status |
|-------|---------|--------|
| axum | 0.8.9 | Current |
| tokio | 1.52.3 | Current |
| rusqlite | 0.39.0 | Current (SQLite 3.51.3) |
| reqwest | 0.13.3 | Current (rustls, no native-tls) |
| rustix | 1.1.4 | Current |
| argon2 | 0.5.3 | Current |
| sha2 | 0.11.0 | Current |
| uuid | 1.23.1 | Current (v7 only) |
| rand | 0.10.1 | Current (ChaCha20 CSPRNG) |
| rustls | 0.23.40 | Current |
| hyper | 1.9.0 | Current |

### Missing: Automated Vulnerability Scanning

No `cargo audit` or `pnpm audit` in CI pipeline. New CVEs will not be detected until a manual audit.

**Fix**: Add `cargo audit` and `pnpm audit` to CI. Consider GitHub Dependabot or Renovate.

---

## Documentation Security Review

### Key Gaps

| Gap | Impact | Priority |
|-----|--------|----------|
| Seccomp implemented but ADR-005 says "deferred" | Operators/contributors have incorrect security posture understanding | P1 |
| No audit logging system designed | Complete repudiation gap | P1 |
| .gitignore lacks secret protection | Accidental secret commits | P1 |
| Bootstrap token permanence documented but unresolved | Permanent skeleton key | P1 |
| No formal threat model document | No unified security reference | P2 |
| Security headers undocumented | Changes without awareness of purpose | P2 |
| Seccomp denylist vs. allowlist rationale missing | Future contributors can't evaluate trade-off | P2 |
| Full Denia binary injected as socket proxy | Large attack surface inside workloads | P2 |
| Multi-node security requirements not defined | Security will be afterthought in future design | P3 |
| Managed Traefik not security-reviewed | Ingress security responsibility not planned | P3 |

---

## Future Security Risks

### Autoscaling (Planned)

The autoscaling design introduces significant new attack surface without security analysis:
- **Scale-to-zero activator**: Connection holding during cold start — DoS via connection exhaustion
- **Orphan adoption**: Adopting leftover cgroups/sockets after crash — race conditions, stale state
- **Replica management**: Inter-replica isolation not addressed
- **Resource ledger**: No protection against manipulation

### Multi-Node (Planned)

Current architecture decisions (SQLite, single admin token, file-based SOPS) may be incompatible with secure multi-node operation. Security prerequisites not defined:
- Inter-node encryption (mTLS)
- Distributed authentication
- Node identity and attestation
- Cross-node secret distribution

### Managed Traefik (Planned)

If Denia takes ownership of Traefik lifecycle, it inherits responsibility for:
- Binary integrity and CVE patching
- ACME certificate storage security (`acme.json` permissions)
- Traefik API/dashboard exposure

---

## Recommended Remediation Roadmap

### Phase 1: Immediate (P1) — Before Production

| # | Action | Effort |
|---|--------|--------|
| C-1 | Fix x86_64 seccomp syscall numbers (use `libc::SYS_*` constants) | 1 hour |
| C-2 | Add seccomp filter to socket proxy for services | 1 hour |
| C-3 | Add digest verification to registry blob pulls | 2 hours |
| C-4 | Author audit log ADR and implement basic audit table | 8 hours |
| H-1 | Add dummy Argon2 verification for non-existent users | 30 min |
| H-2 | Create `PutServiceRequest` DTO with validation | 2 hours |
| H-3 | Custom `Debug` impl for `User` redacting `password_hash` | 15 min |
| H-4 | Set SQLite file permissions to `0o600` after creation | 15 min |
| H-7 | Add defensive `.gitignore` patterns | 5 min |
| H-8 | Implement admin bootstrap with token invalidation | 8 hours |
| H-9 | Fix rate limiter IP extraction for proxied deployments | 2 hours |
| Doc-1 | Update ADR-005 to reflect seccomp implementation | 30 min |

### Phase 2: Short-Term (P2) — Within 2 Weeks

| # | Action | Effort |
|---|--------|--------|
| H-5 | Add missing syscalls to seccomp denylist | 2 hours |
| H-6 | Close inherited FDs in child process before execve | 2 hours |
| M-2 | Increase Argon2 t_cost to 2+ | 30 min |
| M-3 | Add absolute session timeout (e.g., 7 days) | 2 hours |
| M-4 | Block RFC 1918, link-local, loopback in domain verification | 2 hours |
| M-6 | Set default `pids.max` when none specified | 30 min |
| M-7 | Validate `route_key` against strict pattern | 1 hour |
| M-12 | Add `#[serde(skip_serializing)]` to `Session.token` | 15 min |
| Dep-1 | Pin all frontend `latest` dependencies to resolved versions | 30 min |
| Dep-2 | Add `cargo audit` to CI | 30 min |
| Doc-2 | Author formal threat model document | 4 hours |

### Phase 3: Medium-Term (P3) — Within 1 Month

- Add `Permissions-Policy` header
- Add username format validation and max password length
- Fail-closed on secret path validation
- Expand `/proc` masking
- Set explicit `0o600` on log files
- Add graceful SIGTERM before SIGKILL
- Document security headers and session parameters
- Plan per-service UID ranges for multi-tenancy
- Plan minimal static socket-proxy binary

---

## Security Hardening Checklist

- [ ] Fix x86_64 seccomp syscall numbers (C-1)
- [ ] Enable seccomp for service workloads (C-2)
- [ ] Verify registry blob digests on pull (C-3)
- [ ] Implement audit logging (C-4)
- [ ] Fix timing side-channel in login (H-1)
- [ ] Use DTOs for API input validation (H-2)
- [ ] Redact password hashes from Debug output (H-3)
- [ ] Restrict SQLite file permissions (H-4)
- [ ] Add missing syscalls to seccomp denylist (H-5)
- [ ] Close inherited FDs across fork (H-6)
- [ ] Update .gitignore for secrets (H-7)
- [ ] Implement admin bootstrap with token revocation (H-8)
- [ ] Fix rate limiter behind reverse proxy (H-9)
- [ ] Add `cargo audit` and `pnpm audit` to CI
- [ ] Pin all frontend dependencies to exact versions
- [ ] Increase Argon2 iterations to 2+
- [ ] Add absolute session timeout
- [ ] Block internal IPs in domain verification
- [ ] Set default `pids.max` for cgroups
- [ ] Validate Traefik route_key against strict pattern
- [ ] Author formal threat model document
- [ ] Update ADR-005 to reflect seccomp implementation
- [ ] Plan per-service UID ranges for multi-tenancy
- [ ] Plan minimal static socket-proxy binary

---

## Appendix: Positive Security Observations

The following security controls are **well-implemented**:

1. **Parameterized SQL throughout** — Zero SQL injection vectors across the entire data layer
2. **Admin token constant-time comparison** — `subtle::ConstantTimeEq` prevents timing attacks
3. **Tokens stored as SHA-256 hashes** — Raw tokens never persisted
4. **`#[serde(skip_serializing)]` on sensitive fields** — Prevents credential leakage in API responses
5. **RBAC consistently applied** — Every project-scoped endpoint calls `ensure_role()`
6. **Comprehensive security headers** — CSP, HSTS, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, CORP
7. **Body size limited** — `DefaultBodyLimit::max(1MB)` prevents large payload DoS
8. **SecretRef character allowlist** — `[a-zA-Z0-9._-]` prevents path traversal
9. **All 6 Linux namespaces enabled** — user, PID, mount, net, UTS, IPC
10. **pivot_root + umount** — Proper root isolation instead of chroot-only
11. **MS_PRIVATE | MS_REC on `/`** — Prevents mount event leakage
12. **All 41 capabilities dropped** — Comprehensive bounding set reduction
13. **setgroups deny** — Written before gid_map
14. **SUID/SGID stripping** — `mode & 0o0777` on OCI unpack
15. **Decompression bomb limits** — 10GB total, 2GB per-file, 1M file count
16. **NUL byte rejection** — CString validation before fork
17. **O_CLOEXEC sync pipes** — Parent-child pipes close on exec
18. **Privileged test gating** — `#[ignore]` + env var check
19. **TLS enforcement** — `accept_invalid_certificates: false`, `accept_invalid_hostnames: false`
20. **Cryptographic token generation** — 32 bytes (256 bits) of CSPRNG output
21. **Domain hostname validation** — Rejects IPs, paths, ports, uppercase, special chars
22. **SSRF protection in domain verifier** — Blocks internal IP ranges, disables redirects, 5s timeout
23. **Feature flags appropriately scoped** — No unnecessary features enabled
24. **Modern TLS stack** — rustls + aws-lc-rs (FIPS-capable, well-audited)

---

## Appendix: Files Reviewed

### Source Code (95+ files)
`src/main.rs`, `src/lib.rs`, `src/app.rs`, `src/config.rs`, `src/state.rs`, `src/secrets.rs`, `src/rate_limit.rs`, `src/scheduler.rs`, `src/health.rs`, `src/command.rs`, `src/web.rs`, `src/workload_launcher.rs`

`src/api/`: `mod.rs`, `auth.rs`, `users.rs`, `tokens.rs`, `projects.rs`, `services.rs`, `deployments.rs`, `credentials.rs`, `domains.rs`, `ingress.rs`, `registries.rs`, `members.rs`, `jobs.rs`, `observability.rs`, `health.rs`, `error.rs`

`src/auth/`: `mod.rs`, `credentials.rs`, `principal.rs`

`src/runtime/`: `mod.rs`, `linux.rs`, `validation.rs`, `fake.rs`, `fs_helpers.rs`, `plan.rs`, `runtime_trait.rs`, `error.rs`

`src/syscall/`: `mod.rs`, `ns.rs`, `seccomp.rs`, `caps.rs`, `signal.rs`, `chown.rs`

`src/oci/`: `mod.rs`, `registry.rs`, `unpack.rs`, `layout.rs`, `credentials.rs`, `config.rs`

`src/artifacts/`: `mod.rs`, `acquirer.rs`

`src/repo/`: `mod.rs`, `error.rs`, `mock.rs`, `sqlite/mod.rs`, `sqlite/pool.rs`, `sqlite/projects.rs`, `sqlite/services.rs`, `sqlite/deployments.rs`, `sqlite/credentials.rs`, `sqlite/registries.rs`, `sqlite/tokens.rs`, `sqlite/users.rs`, `sqlite/domains.rs`, `sqlite/jobs.rs`

`src/deploy/`: `mod.rs`, `coordinator.rs`, `routes.rs`, `error.rs`

`src/domain/`: `mod.rs`, `service.rs`, `service_domain.rs`, `user.rs`, `deployment.rs`, `project.rs`, `registry.rs`, `credential.rs`, `job.rs`, `error.rs`

`src/verification/`: `mod.rs`, `http.rs`, `verifier.rs`, `validation.rs`, `error.rs`

`src/ingress/`: `mod.rs`, `bridge.rs`, `socket_proxy.rs`, `traefik.rs`

`src/observability/`: `mod.rs`, `logs.rs`, `access_log.rs`, `metrics.rs`, `node_metrics.rs`

### Tests
`tests/linux_runtime_privileged.rs`, `tests/deploy_orchestration.rs`, `tests/repo_contract.rs`, `tests/backend_contract.rs`, `tests/domain_verification.rs`

### Documentation
`docs/adr/`: `README.md`, `001` through `015`
`docs/superpowers/specs/`: 15 design specification files
`docs/superpowers/plans/`: 17 implementation plan files

### Configuration
`Cargo.toml`, `Cargo.lock`, `web/package.json`, `web/pnpm-lock.yaml`, `.gitignore`, `AGENTS.md`, `README.md`, `PRODUCT.md`, `DESIGN.md`, `TODO.md`
