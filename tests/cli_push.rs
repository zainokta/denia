//! Integration tests for `denia push`. Drives the upload-and-deploy flow
//! against an httpmock server using assert_cmd.

use assert_cmd::Command;
use httpmock::prelude::*;
use predicates::prelude::*;

/// Set up a temporary directory with a minimal `.denia` manifest and a
/// `Dockerfile`, seeding a client.toml pointing at `server_url`.
fn setup_temp_dir(server_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();

    // Write the .denia manifest.
    std::fs::write(
        dir.path().join(".denia"),
        "project = \"default\"\nservice = \"api\"\n",
    )
    .unwrap();

    // Write a minimal Dockerfile.
    std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();

    // Write client.toml pointing at the mock server.
    let cfg_path = dir.path().join("client.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "active = \"default\"\n\n[profiles.default]\nurl = \"{server_url}\"\ntoken = \"testtoken\"\n"
        ),
    )
    .unwrap();

    (dir, cfg_path)
}

#[test]
fn push_no_follow_existing_service_succeeds() {
    let server = MockServer::start();
    let pid = "01900000-0000-7000-8000-000000000001";
    let sid = "01900000-0000-7000-8000-000000000002";

    let _projects = server.mock(|when, then| {
        when.method(GET).path("/v1/projects");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"[{{"id":"{pid}","name":"default"}}]"#));
    });

    let _services = server.mock(|when, then| {
        when.method(GET).path("/v1/services");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"[{{"id":"{sid}","name":"api","project_id":"{pid}"}}]"#
            ));
    });

    let upload_mock = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/v1/services/{sid}/uploads"))
            .header("content-type", "application/zstd");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"upload_id":"u1","expires_at":"2026-01-01T00:00:00Z"}"#);
    });

    let deploy_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/deployments")
            .body_includes(r#""source":"upload""#)
            .body_includes(r#""dockerfile_path""#)
            .body_includes(r#""context_path""#);
        then.status(202)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"id":"d1","service_id":"{sid}","status":"Pending","created_at":"2026-01-01T00:00:00Z"}}"#
            ));
    });

    let (dir, cfg_path) = setup_temp_dir(&server.base_url());

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args([
            "push",
            "--no-follow",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deployment d1 created"));

    upload_mock.assert_calls(1);
    deploy_mock.assert_calls(1);
}

#[test]
fn push_non_default_context_sends_archive_root_context_path() {
    let server = MockServer::start();
    let pid = "01900000-0000-7000-8000-000000000011";
    let sid = "01900000-0000-7000-8000-000000000012";

    let _projects = server.mock(|when, then| {
        when.method(GET).path("/v1/projects");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"[{{"id":"{pid}","name":"default"}}]"#));
    });

    let _services = server.mock(|when, then| {
        when.method(GET).path("/v1/services");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(
                r#"[{{"id":"{sid}","name":"api","project_id":"{pid}"}}]"#
            ));
    });

    let _upload = server.mock(|when, then| {
        when.method(POST)
            .path(format!("/v1/services/{sid}/uploads"))
            .header("content-type", "application/zstd");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"upload_id":"u1","expires_at":"2026-01-01T00:00:00Z"}"#);
    });

    let deploy_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/deployments")
            .body_includes(r#""source":"upload""#)
            .body_includes(r#""dockerfile_path":"Dockerfile""#)
            .body_includes(r#""context_path":".""#);
        then.status(202)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"id":"d1","service_id":"{sid}","status":"Pending","created_at":"2026-01-01T00:00:00Z"}}"#
            ));
    });

    let (dir, cfg_path) = setup_temp_dir(&server.base_url());
    std::fs::write(
        dir.path().join(".denia"),
        "project = \"default\"\nservice = \"api\"\ncontext = \"app\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app").join("Dockerfile"), "FROM scratch\n").unwrap();

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args([
            "push",
            "--no-follow",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deployment d1 created"));

    deploy_mock.assert_calls(1);
}

#[test]
fn push_appears_in_help() {
    Command::cargo_bin("denia")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("push"));
}

#[test]
fn push_fails_without_denia_manifest_and_no_flags() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("client.toml");
    // Write a dummy client config so the profile load doesn't fail first.
    std::fs::write(
        &cfg_path,
        "active = \"default\"\n\n[profiles.default]\nurl = \"http://127.0.0.1:19999\"\ntoken = \"t\"\n",
    )
    .unwrap();

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args(["push", "--path", dir.path().to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn push_create_guard_refuses_without_control_domain() {
    let server = MockServer::start();
    let pid = "01900000-0000-7000-8000-000000000003";

    let _projects = server.mock(|when, then| {
        when.method(GET).path("/v1/projects");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"[{{"id":"{pid}","name":"myproject"}}]"#));
    });

    let _services = server.mock(|when, then| {
        when.method(GET).path("/v1/services");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"[]"#);
    });

    // Node info returns no control_domain.
    let _node = server.mock(|when, then| {
        when.method(GET).path("/v1/node");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"control_domain":null}"#);
    });

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".denia"),
        "project = \"myproject\"\nservice = \"mysvc\"\n[create]\nport = 8080\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
    let cfg_path = dir.path().join("client.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "active = \"default\"\n\n[profiles.default]\nurl = \"{}\"\ntoken = \"t\"\n",
            server.base_url()
        ),
    )
    .unwrap();

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args([
            "push",
            "--create",
            "--no-follow",
            "--path",
            dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("control domain"));
}

#[test]
fn push_service_not_found_without_create_flag() {
    let server = MockServer::start();
    let pid = "01900000-0000-7000-8000-000000000004";

    let _projects = server.mock(|when, then| {
        when.method(GET).path("/v1/projects");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"[{{"id":"{pid}","name":"default"}}]"#));
    });

    let _services = server.mock(|when, then| {
        when.method(GET).path("/v1/services");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"[]"#);
    });

    let (dir, cfg_path) = setup_temp_dir(&server.base_url());

    Command::cargo_bin("denia")
        .unwrap()
        .env("DENIA_CLIENT_CONFIG", &cfg_path)
        .args(["push", "--path", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"))
        .stderr(predicate::str::contains("--create"));
}
