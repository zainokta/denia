# Managed Zot Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Zot a pinned, Denia-managed host dependency for the hosted OCI registry path and prepare the public `/v2` boundary to use it without weakening Denia authentication or runtime isolation.

**Architecture:** `denia setup` installs and verifies Zot, writes a loopback-only config and dedicated systemd unit, and starts Zot before Denia. Denia remains the public auth/RBAC edge; external project-scoped registry pulls and rootfs unpacking remain in-process until their credential semantics can be migrated safely.

**Tech Stack:** Rust 2024, systemd, Zot 2.1.20, SHA-256 verification, Axum/reqwest where proxying is introduced.

**Spec:** `docs/superpowers/specs/2026-08-24-managed-zot-registry-design.md`

## Global Constraints

- Zot version is pinned to `2.1.20`.
- Zot listens only on `127.0.0.1:5000`.
- Zot storage is `/var/lib/denia/zot`.
- The Zot binary is installed as `/usr/local/bin/zot` only after SHA-256 verification.
- Denia remains the public `/v2` authorization edge.
- Existing project-scoped private upstream pulls remain on Denia's current puller.
- Existing OCI rootfs unpacking remains unchanged.

---

### Task 1: Zot installer and configuration

**Files:**
- Create: `src/cli/common/zot.rs`
- Modify: `src/cli/common/mod.rs`
- Test: module tests in `src/cli/common/zot.rs`

**Interfaces:**
- Produces: `ensure_installed()`, `write_config()`, `verify_config()`, `render_config()`, `release_url_for_arch()`, and version/checksum constants.

- [ ] **Step 1:** Add tests asserting the pinned amd64/arm64 release URLs and hashes and the loopback-only rendered config.
- [ ] **Step 2:** Verify the tests would fail because the Zot module/functions do not exist yet.
- [ ] **Step 3:** Implement architecture mapping, version detection, HTTPS download through `curl`, SHA-256 verification, atomic install, directory ownership, config rendering/writing, and `zot verify`.
- [ ] **Step 4:** Run the focused module tests and confirm they pass.
- [ ] **Step 5:** Commit the installer/configuration change.

### Task 2: Zot systemd lifecycle

**Files:**
- Create: `src/templates/zot.service.in`
- Modify: `src/cli/common/systemd.rs`
- Modify: `src/templates/denia.service.in`
- Test: module tests in `src/cli/common/systemd.rs`

**Interfaces:**
- Produces: `render_zot_unit()` and `write_zot_unit()`.

- [ ] **Step 1:** Add tests that require a `denia:denia` Zot service, `/usr/local/bin/zot serve /etc/denia/zot.json`, and Denia's `After/Wants/Requires=zot.service` dependency.
- [ ] **Step 2:** Verify the tests fail before the new renderer/template exists.
- [ ] **Step 3:** Add the Zot unit template and renderer/writer, then update the Denia unit dependency.
- [ ] **Step 4:** Run focused systemd tests and confirm they pass.
- [ ] **Step 5:** Commit the systemd lifecycle change.

### Task 3: Setup orchestration

**Files:**
- Modify: `src/cli/setup.rs`
- Test: module tests in `src/cli/setup.rs`

**Interfaces:**
- Consumes: Task 1 Zot installer/config functions and Task 2 systemd functions.

- [ ] **Step 1:** Add plan-order tests requiring Zot install/config/unit before daemon reload and Zot activation before Denia activation.
- [ ] **Step 2:** Verify the plan-order tests fail on the current setup plan.
- [ ] **Step 3:** Add idempotent setup steps to install Zot, write/verify config, write its unit, enable it, and wait for it before Denia.
- [ ] **Step 4:** Run setup tests and confirm they pass.
- [ ] **Step 5:** Commit setup integration.

### Task 4: Documentation and compatibility boundary

**Files:**
- Modify: `README.md` if installation prerequisites are documented there.
- Create or update ADR documenting that Zot replaces hosted-registry service/storage responsibility while external private pulls and unpacking stay in Denia.

- [ ] **Step 1:** Document that operators do not install Zot manually; `sudo denia setup` manages it.
- [ ] **Step 2:** Document the pinned binary, loopback port, storage path, and the private-upstream credential boundary.
- [ ] **Step 3:** Commit documentation.

### Task 5: Verification and pull request

**Files:** none required.

- [ ] **Step 1:** Inspect the full branch diff for accidental public Zot exposure or weakened registry auth.
- [ ] **Step 2:** Run available tests/build checks. If the execution environment cannot materialize the repository, record that limitation explicitly and use GitHub diff/status inspection instead of claiming local tests passed.
- [ ] **Step 3:** Open a pull request against `master` with architecture, security boundary, installation behavior, and verification notes.
