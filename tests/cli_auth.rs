//! Integration test for `denia auth`. Drives the full login → token-mint flow
//! against an httpmock server, using `--password-stdin` to avoid tty interaction.

use assert_cmd::Command;
use httpmock::prelude::*;
use predicates::prelude::*;

#[test]
fn auth_saves_profile_and_prints_confirmation() {
    let server = MockServer::start();

    let _healthz = server.mock(|when, then| {
        when.method(GET).path("/healthz");
        then.status(200).body("ok");
    });
    let _login = server.mock(|when, then| {
        when.method(POST).path("/v1/auth/login");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"token":"sess","expires_at":"2026-01-01T00:00:00Z"}"#);
    });
    let _mint = server.mock(|when, then| {
        when.method(POST).path("/v1/api-tokens");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"i","name":"denia-cli","token":"longlived"}"#);
    });
    let _me = server.mock(|when, then| {
        when.method(GET).path("/v1/me");
        then.status(200)
            .header("content-type", "application/json")
            .body("{}");
    });

    let temp = tempfile::tempdir().unwrap();
    let cfg_path = temp.path().join("client.toml");

    let base_url = server.base_url();

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args([
            "auth",
            "--url",
            &base_url,
            "--username",
            "u",
            "--password-stdin",
            "--profile",
            "test",
        ])
        .write_stdin("pw\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Authenticated as u"))
        .stdout(predicate::str::contains("saved profile 'test'"));

    let contents = std::fs::read_to_string(&cfg_path).expect("client.toml should exist");
    assert!(
        contents.contains("longlived"),
        "profile file should contain the minted token"
    );
    assert!(
        contents.contains("active = \"test\""),
        "profile file should set active = \"test\""
    );
    // Paranoia: session token must NOT be persisted.
    assert!(
        !contents.contains("sess"),
        "session token must not be written to disk"
    );
}

#[test]
fn auth_appears_in_top_level_help() {
    Command::cargo_bin("denia")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("auth"));
}

#[test]
fn auth_unreachable_host_errors_clearly() {
    let temp = tempfile::tempdir().unwrap();
    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", temp.path().join("client.toml"))
        .args([
            "auth",
            "--url",
            "http://127.0.0.1:19999",
            "--username",
            "u",
            "--password-stdin",
        ])
        .write_stdin("pw\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot reach Denia"));
}
