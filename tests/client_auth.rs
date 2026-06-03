use assert_cmd::Command;
use httpmock::prelude::*;

#[tokio::test]
async fn auth_creates_profile_from_login() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/auth/login");
            then.status(200).json_body_obj(&serde_json::json!({
                "token": "session-token",
                "expires_at": "2026-06-03T00:00:00Z"
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/api-tokens");
            then.status(201).json_body_obj(&serde_json::json!({
                "id": "01976f2e-0000-7000-8000-000000000000",
                "name": "denia-cli-test",
                "token": "api-token"
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(GET).path("/v1/me");
            then.status(200).json_body_obj(&serde_json::json!({
                "principal": {"kind":"user","user":{"id":"01976f2e-0000-7000-8000-000000000001","username":"zain","created_at":"2026-06-03T00:00:00Z"}},
                "is_super_admin": false,
                "admin_initialized": true,
                "memberships": []
            }));
        })
        .await;

    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("profiles.toml");
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.env("DENIA_CLIENT_CONFIG", &config_path)
        .arg("auth")
        .arg("--url")
        .arg(server.url(""))
        .arg("--username")
        .arg("zain")
        .arg("--password-stdin")
        .write_stdin("secret\n")
        .assert()
        .success();

    let saved = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved.contains("api-token"), "token must be persisted");
    assert!(saved.contains("active = \"default\""));
}
