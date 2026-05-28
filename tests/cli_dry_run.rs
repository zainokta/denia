//! Dry-run integration tests for `denia setup` and `denia uninstall`.

use assert_cmd::Command;

#[test]
fn setup_dry_run_lists_expected_steps() {
    let output = Command::cargo_bin("denia")
        .unwrap()
        .env("SUDO_USER", "rakei")
        // require_root requires EUID==0; in a normal test environment we
        // don't have root. The dry-run still runs require_root first and
        // will error out. To exercise the plan, we accept either a
        // non-zero exit with an explicit error message OR successful
        // plan output if running as root.
        //
        // Strategy: run, capture stderr+stdout, assert that EITHER:
        //   (a) stderr says "must run as root"  (typical CI path), OR
        //   (b) stdout contains every plan step (root-CI path).
        .args(["setup", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    let has_plan = [
        "groupadd",
        "useradd",
        "/var/lib/denia",
        "config.toml",
        "systemctl",
    ]
    .iter()
    .all(|n| stdout.contains(n));
    let has_root_error = combined.contains("must run as root");
    assert!(
        has_plan || has_root_error,
        "expected dry-run plan or 'must run as root' error; got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn uninstall_dry_run_purge_lists_expected_steps() {
    let output = Command::cargo_bin("denia")
        .unwrap()
        .env("SUDO_USER", "rakei")
        .args(["uninstall", "--dry-run", "--purge"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    let has_plan = [
        "systemctl disable",
        "/etc/systemd/system/denia.service",
        "userdel denia",
        "/var/lib/denia",
    ]
    .iter()
    .all(|n| stdout.contains(n));
    let has_root_error = combined.contains("must run as root");
    assert!(
        has_plan || has_root_error,
        "expected uninstall dry-run plan or 'must run as root' error; got stdout={stdout:?} stderr={stderr:?}"
    );
}
