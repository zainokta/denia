use denia::cli::client::manifest::DeniaManifest;

#[test]
fn parse_valid_git_manifest() {
    let raw = r#"
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
tls_enabled = true
"#;
    let manifest = DeniaManifest::parse(raw).unwrap();
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.project, "default");
    assert_eq!(manifest.service, "api");
    assert_eq!(manifest.source.remote, "origin");
    assert_eq!(manifest.runtime.internal_port, 8080);
}

#[test]
fn reject_unsupported_manifest_version() {
    let raw = r#"
version = 2
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
"#;
    let err = DeniaManifest::parse(raw).unwrap_err().to_string();
    assert!(err.contains("unsupported .denia version: 2"));
}

#[test]
fn reject_empty_service_name() {
    let raw = r#"
version = 1
project = "default"
service = ""
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
"#;
    let err = DeniaManifest::parse(raw).unwrap_err().to_string();
    assert!(err.contains("service must not be empty"));
}
