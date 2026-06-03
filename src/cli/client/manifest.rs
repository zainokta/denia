//! `.denia` project manifest: a small TOML file in a project directory naming
//! the project and service so `denia console` (and future client commands) can
//! resolve a target without explicit flags.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct DeniaManifest {
    pub project: String,
    pub service: String,
}

impl DeniaManifest {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        toml::from_str(raw).map_err(|error| anyhow::anyhow!("invalid .denia manifest: {error}"))
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
}
