//! `.denia` project manifest: a small TOML file in a project directory naming
//! the project and service so `denia console` (and future client commands) can
//! resolve a target without explicit flags.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeniaManifest {
    pub project: String,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<CreateDefaults>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDefaults {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_path: Option<String>,
}

impl DeniaManifest {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        toml::from_str(raw).map_err(|error| anyhow::anyhow!("invalid .denia manifest: {error}"))
    }

    pub fn dockerfile(&self) -> &str {
        self.dockerfile.as_deref().unwrap_or("Dockerfile")
    }

    pub fn context(&self) -> &str {
        self.context.as_deref().unwrap_or(".")
    }

    pub fn write_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_and_service() {
        let manifest = DeniaManifest::parse("project = \"team\"\nservice = \"web\"\n").unwrap();
        assert_eq!(manifest.project, "team");
        assert_eq!(manifest.service, "web");
    }

    #[test]
    fn rejects_missing_fields() {
        assert!(DeniaManifest::parse("project = \"team\"\n").is_err());
    }

    #[test]
    fn build_fields_default_when_absent() {
        let m = DeniaManifest::parse("project=\"p\"\nservice=\"s\"\n").unwrap();
        assert_eq!(m.dockerfile(), "Dockerfile");
        assert_eq!(m.context(), ".");
        assert!(m.create.is_none());
    }

    #[test]
    fn parses_build_and_create_blocks() {
        let raw = "project=\"p\"\nservice=\"s\"\ndockerfile=\"docker/Dockerfile\"\ncontext=\"app\"\n[create]\nport=8080\nhealth_path=\"/healthz\"\n";
        let m = DeniaManifest::parse(raw).unwrap();
        assert_eq!(m.dockerfile(), "docker/Dockerfile");
        assert_eq!(m.context(), "app");
        assert_eq!(m.create.as_ref().unwrap().port, 8080);
    }

    #[test]
    fn write_to_round_trip() {
        let original = DeniaManifest {
            project: "myproject".to_string(),
            service: "myservice".to_string(),
            dockerfile: Some("docker/Dockerfile".to_string()),
            context: Some("app".to_string()),
            create: Some(CreateDefaults {
                port: 3000,
                health_path: Some("/healthz".to_string()),
            }),
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        original.write_to(tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path()).unwrap();
        let parsed = DeniaManifest::parse(&contents).unwrap();
        assert_eq!(parsed.project, original.project);
        assert_eq!(parsed.service, original.service);
        assert_eq!(parsed.dockerfile(), "docker/Dockerfile");
        assert_eq!(parsed.context(), "app");
        let create = parsed.create.as_ref().unwrap();
        assert_eq!(create.port, 3000);
        assert_eq!(create.health_path.as_deref(), Some("/healthz"));
    }
}
