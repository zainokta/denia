#![cfg(feature = "server")]
use assert_cmd::Command;

#[test]
fn help_shows_client_and_server_groups() {
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("auth"))
        .stdout(predicates::str::contains("push"))
        .stdout(predicates::str::contains("server"));
}
