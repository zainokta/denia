use std::{env, net::SocketAddr, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub admin_token: String,
    pub database_path: PathBuf,
    pub data_dir: PathBuf,
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
        let traefik_dynamic_config_path = env::var("DENIA_TRAEFIK_DYNAMIC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/traefik/dynamic/denia.yml"));

        Ok(Self {
            bind_addr,
            admin_token,
            database_path,
            data_dir,
            traefik_dynamic_config_path,
        })
    }

    pub fn for_test(admin_token: impl Into<String>) -> Self {
        Self {
            bind_addr: "127.0.0.1:0".parse().expect("valid test bind addr"),
            admin_token: admin_token.into(),
            database_path: PathBuf::from(":memory:"),
            data_dir: PathBuf::from("/tmp/denia-test"),
            traefik_dynamic_config_path: PathBuf::from("/tmp/denia-traefik.yml"),
        }
    }
}
