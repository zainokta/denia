use std::{env, net::SocketAddr, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub admin_token: String,
    pub database_path: PathBuf,
    pub data_dir: PathBuf,
    pub buildkit_binary: PathBuf,
    pub sops_binary: PathBuf,
    pub registry_pull_binary: PathBuf,
    pub runtime_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub log_dir: PathBuf,
    pub traefik_dynamic_config_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DENIA_ADMIN_TOKEN must be set")]
    MissingAdminToken,
    #[error("invalid DENIA_BIND_ADDR: {0}")]
    InvalidBindAddr(#[from] std::net::AddrParseError),
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let admin_token =
            env::var("DENIA_ADMIN_TOKEN").map_err(|_| ConfigError::MissingAdminToken)?;
        let bind_addr = env::var("DENIA_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:7180".to_string())
            .parse()?;
        let data_dir = PathBuf::from(
            env::var("DENIA_DATA_DIR").unwrap_or_else(|_| "/var/lib/denia".to_string()),
        );
        let database_path = env::var("DENIA_DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("denia.sqlite3"));
        let buildkit_binary = PathBuf::from(
            env::var("DENIA_BUILDKIT_BINARY").unwrap_or_else(|_| "buildctl".to_string()),
        );
        let sops_binary =
            PathBuf::from(env::var("DENIA_SOPS_BINARY").unwrap_or_else(|_| "sops".to_string()));
        let registry_pull_binary = PathBuf::from(
            env::var("DENIA_REGISTRY_PULL_BINARY").unwrap_or_else(|_| "skopeo".to_string()),
        );
        let runtime_dir = data_dir.join("runtime");
        let artifact_dir = data_dir.join("artifacts");
        let log_dir = data_dir.join("logs");
        let traefik_dynamic_config_path = env::var("DENIA_TRAEFIK_DYNAMIC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/traefik/dynamic/denia.yml"));

        Ok(Self {
            bind_addr,
            admin_token,
            database_path,
            data_dir,
            buildkit_binary,
            sops_binary,
            registry_pull_binary,
            runtime_dir,
            artifact_dir,
            log_dir,
            traefik_dynamic_config_path,
        })
    }

    pub fn for_test(admin_token: impl Into<String>) -> Self {
        let data_dir = PathBuf::from("/tmp/denia-test");
        Self {
            bind_addr: "127.0.0.1:0".parse().expect("valid test bind addr"),
            admin_token: admin_token.into(),
            database_path: PathBuf::from(":memory:"),
            data_dir: data_dir.clone(),
            buildkit_binary: PathBuf::from("buildctl"),
            sops_binary: PathBuf::from("sops"),
            registry_pull_binary: PathBuf::from("skopeo"),
            runtime_dir: data_dir.join("runtime"),
            artifact_dir: data_dir.join("artifacts"),
            log_dir: data_dir.join("logs"),
            traefik_dynamic_config_path: PathBuf::from("/tmp/denia-traefik.yml"),
        }
    }
}
