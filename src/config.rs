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
    pub oci_unpack_binary: PathBuf,
    pub runtime_dir: PathBuf,
    pub cgroup_root: PathBuf,
    pub artifact_dir: PathBuf,
    pub log_dir: PathBuf,
    pub bridge_start_port: u16,
    pub traefik_dynamic_config_path: PathBuf,
    pub userns_base: u32,
    pub userns_size: u32,
    pub setpriv_binary: PathBuf,
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
        let oci_unpack_binary = PathBuf::from(
            env::var("DENIA_OCI_UNPACK_BINARY").unwrap_or_else(|_| "umoci".to_string()),
        );
        let runtime_dir = data_dir.join("runtime");
        let cgroup_root = env::var("DENIA_CGROUP_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/sys/fs/cgroup/denia"));
        let artifact_dir = data_dir.join("artifacts");
        let log_dir = data_dir.join("logs");
        let bridge_start_port = env::var("DENIA_BRIDGE_START_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(19_000);
        let traefik_dynamic_config_path = env::var("DENIA_TRAEFIK_DYNAMIC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/traefik/dynamic/denia.yml"));
        let userns_base = env::var("DENIA_USERNS_BASE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100000);
        let userns_size = env::var("DENIA_USERNS_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(65536);
        let setpriv_binary = PathBuf::from(
            env::var("DENIA_SETPRIV_BINARY").unwrap_or_else(|_| "setpriv".to_string()),
        );

        Ok(Self {
            bind_addr,
            admin_token,
            database_path,
            data_dir,
            buildkit_binary,
            sops_binary,
            registry_pull_binary,
            oci_unpack_binary,
            runtime_dir,
            cgroup_root,
            artifact_dir,
            log_dir,
            bridge_start_port,
            traefik_dynamic_config_path,
            userns_base,
            userns_size,
            setpriv_binary,
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
            oci_unpack_binary: PathBuf::from("umoci"),
            runtime_dir: data_dir.join("runtime"),
            cgroup_root: data_dir.join("cgroup"),
            artifact_dir: data_dir.join("artifacts"),
            log_dir: data_dir.join("logs"),
            bridge_start_port: 19_000,
            traefik_dynamic_config_path: PathBuf::from("/tmp/denia-traefik.yml"),
            userns_base: 100000,
            userns_size: 65536,
            setpriv_binary: PathBuf::from("setpriv"),
        }
    }
}
