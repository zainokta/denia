//! Round-trip the `denia setup` happy path against a tempdir-rooted layout.
//! Requires root + cgroup v2 + userns. Opt-in via:
//!   DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test cli_setup_privileged -- --ignored
//!
//! The body is TODO: implementing this safely requires either chroot-style
//! redirection of `/var/lib/denia` and `/etc/systemd/system/denia.service`
//! or a containerized harness. Tracked as a follow-up to ADR-024.

#[test]
#[ignore]
fn setup_creates_user_dirs_unit_and_starts_service() {
    if std::env::var("DENIA_RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping privileged cli_setup test; \
             set DENIA_RUN_PRIVILEGED_TESTS=1 to enable"
        );
        return;
    }
    // SAFETY: requires root (CAP_SYS_ADMIN-equivalent), cgroup v2 unified
    // hierarchy, and unprivileged_userns_clone=1. Refuse to run if any
    // precondition is missing.
    unimplemented!("end-to-end `denia setup` round-trip lands in a follow-up task");
}
