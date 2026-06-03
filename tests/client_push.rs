use assert_cmd::Command as AssertCommand;
use denia::cli::client::profile::{ClientConfig, ClientProfile};
use httpmock::prelude::*;
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = r#"
version = 1
project = "default"
service = "api"

[source]
type = "git"
remote = "origin"
dockerfile = "Dockerfile"
context = "."
git_credential_ref = "deploy-key"

[runtime]
internal_port = 8080

[health]
path = "/"
timeout_seconds = 5

[limits]
cpu_millis = 500
memory_bytes = 536870912

[ingress]
domains = ["api.example.com"]
tls_enabled = false
"#;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Init a repo with a committed `.denia`, wired to a bare remote with HEAD
/// pushed. Returns nothing; the repo dir is the caller's `repo`.
fn init_pushed_repo(repo: &Path, remote: &Path) {
    Command::new("git")
        .args(["init", "-q"])
        .arg(remote)
        .arg("--bare")
        .status()
        .unwrap();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "user.name", "tester"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".denia"), MANIFEST).unwrap();
    std::fs::write(repo.join("Dockerfile"), "FROM scratch\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);
    git(repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(repo, &["push", "-q", "origin", "HEAD"]);
}

fn write_profile(config_path: &Path, url: String) {
    ClientConfig {
        active: "default".into(),
        profiles: vec![ClientProfile {
            name: "default".into(),
            url,
            token: "api-token".into(),
        }],
    }
    .save_to(config_path)
    .unwrap();
}

#[test]
fn missing_denia_errors() {
    let dir = tempfile::tempdir().unwrap();
    AssertCommand::cargo_bin("denia")
        .unwrap()
        .current_dir(dir.path())
        .env("DENIA_CLIENT_CONFIG", dir.path().join("profiles.toml"))
        .arg("push")
        .assert()
        .failure()
        .stderr(predicates::str::contains("missing .denia"));
}

#[test]
fn unpushed_branch_errors() {
    let repo = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    init_pushed_repo(repo.path(), &remote.path().join("remote.git"));
    // A new local commit that is not pushed makes HEAD diverge from origin.
    std::fs::write(repo.path().join("extra.txt"), "x\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "unpushed"]);

    let config_path = repo.path().join("profiles.toml");
    write_profile(&config_path, "http://127.0.0.1:9".into());

    AssertCommand::cargo_bin("denia")
        .unwrap()
        .current_dir(repo.path())
        .env("DENIA_CLIENT_CONFIG", &config_path)
        .arg("push")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not pushed"));
}

#[tokio::test]
async fn push_happy_path() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/v1/projects");
            then.status(200).json_body_obj(&serde_json::json!([
                {"id":"01976f2e-0000-7000-8000-0000000000aa","name":"default","description":null,"shared_env":[],"default_resource_limits":null,"created_at":"2026-06-03T00:00:00Z"}
            ]));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/v1/services");
            then.status(200).json_body_obj(&serde_json::json!([]));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/services");
            then.status(200).json_body_obj(&serde_json::json!({
                "id":"01976f2e-0000-7000-8000-0000000000bb",
                "name":"api",
                "project_id":"01976f2e-0000-7000-8000-0000000000aa"
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/deployments");
            then.status(202).json_body_obj(&serde_json::json!({
                "id":"01976f2e-0000-7000-8000-0000000000cc",
                "service_id":"01976f2e-0000-7000-8000-0000000000bb",
                "status":"Pending"
            }));
        })
        .await;

    let repo = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    init_pushed_repo(repo.path(), &remote.path().join("remote.git"));
    let config_path = repo.path().join("profiles.toml");
    write_profile(&config_path, server.url(""));

    AssertCommand::cargo_bin("denia")
        .unwrap()
        .current_dir(repo.path())
        .env("DENIA_CLIENT_CONFIG", &config_path)
        .arg("push")
        .assert()
        .success()
        .stdout(predicates::str::contains("Deployment"));
}
