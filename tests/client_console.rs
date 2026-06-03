use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn console_help_is_available() {
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.arg("console")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--replica"))
        .stdout(predicate::str::contains("--project"));
}

#[test]
fn console_appears_in_top_level_help() {
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("console"));
}

#[test]
fn console_without_profile_errors_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.env("DENIA_CLIENT_CONFIG", temp.path().join("missing.toml"))
        .arg("console")
        .arg("web")
        .assert()
        .failure()
        .stderr(predicate::str::contains("profile"));
}
