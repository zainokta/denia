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
    pub socket_proxy_binary: PathBuf,
    pub runtime_dir: PathBuf,
    pub cgroup_root: PathBuf,
    pub artifact_dir: PathBuf,
    pub log_dir: PathBuf,
    pub bridge_start_port: u16,
    pub traefik_dynamic_config_path: PathBuf,
    pub userns_base: u32,
    pub userns_size: u32,
    pub acme_resolver: String,
    pub control_domain: Option<String>,
    pub control_tls: bool,
    pub node_disk_path: PathBuf,
    pub traefik_image: String,
    pub acme_email: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub traefik_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DENIA_ADMIN_TOKEN must be set")]
    MissingAdminToken,
    #[error("DENIA_ADMIN_TOKEN must be at least 32 characters long")]
    AdminTokenTooShort,
    #[error("invalid DENIA_BIND_ADDR: {0}")]
    InvalidBindAddr(#[from] std::net::AddrParseError),
    #[error("DENIA_ACME_EMAIL must be set when any service uses TLS")]
    AcmeEmailRequired,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let admin_token =
            env::var("DENIA_ADMIN_TOKEN").map_err(|_| ConfigError::MissingAdminToken)?;
        if admin_token.len() < 32 {
            return Err(ConfigError::AdminTokenTooShort);
        }
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
        let socket_proxy_binary = env::var("DENIA_SOCKET_PROXY_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_exe().unwrap_or_else(|_| "denia".into()));
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
        let traefik_dir = data_dir.join("traefik");
        let traefik_image = env::var("DENIA_TRAEFIK_IMAGE")
            .unwrap_or_else(|_| "docker.io/library/traefik:v3.3".to_string());
        let acme_email = env::var("DENIA_ACME_EMAIL").ok().filter(|v| !v.is_empty());
        let http_port = env::var("DENIA_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80);
        let https_port = env::var("DENIA_HTTPS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(443);
        let traefik_dynamic_config_path = env::var("DENIA_TRAEFIK_DYNAMIC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| traefik_dir.join("dynamic/denia.yml"));
        let userns_base = env::var("DENIA_USERNS_BASE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100000);
        let userns_size = env::var("DENIA_USERNS_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(65536);
        let acme_resolver = env::var("DENIA_ACME_RESOLVER").unwrap_or_else(|_| "le".to_string());
        let control_domain = env::var("DENIA_CONTROL_DOMAIN").ok();
        let control_tls = env::var("DENIA_CONTROL_TLS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let node_disk_path = env::var("DENIA_NODE_DISK_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.clone());

        Ok(Self {
            bind_addr,
            admin_token,
            database_path,
            data_dir,
            buildkit_binary,
            sops_binary,
            socket_proxy_binary,
            runtime_dir,
            cgroup_root,
            artifact_dir,
            log_dir,
            bridge_start_port,
            traefik_dynamic_config_path,
            userns_base,
            userns_size,
            acme_resolver,
            control_domain,
            control_tls,
            node_disk_path,
            traefik_image,
            acme_email,
            http_port,
            https_port,
            traefik_dir,
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
            socket_proxy_binary: PathBuf::from("denia"),
            runtime_dir: data_dir.join("runtime"),
            cgroup_root: data_dir.join("cgroup"),
            artifact_dir: data_dir.join("artifacts"),
            log_dir: data_dir.join("logs"),
            bridge_start_port: 19_000,
            traefik_dir: data_dir.join("traefik"),
            traefik_dynamic_config_path: data_dir.join("traefik/dynamic/denia.yml"),
            userns_base: 100000,
            userns_size: 65536,
            acme_resolver: "le".to_string(),
            control_domain: None,
            control_tls: false,
            node_disk_path: data_dir,
            traefik_image: "docker.io/library/traefik:v3.3".to_string(),
            acme_email: None,
            http_port: 80,
            https_port: 443,
        }
    }

    pub fn require_acme_email(&self, tls_in_use: bool) -> Result<(), ConfigError> {
        if tls_in_use && self.acme_email.is_none() {
            return Err(ConfigError::AcmeEmailRequired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod managed_traefik_tests {
    use super::*;

    fn base() -> AppConfig {
        AppConfig::for_test("0123456789012345678901234567890123")
    }

    #[test]
    fn traefik_dir_under_data_dir() {
        let c = base();
        assert_eq!(c.traefik_dir, c.data_dir.join("traefik"));
    }

    #[test]
    fn dynamic_config_defaults_under_traefik_dir() {
        let c = base();
        assert_eq!(
            c.traefik_dynamic_config_path,
            c.data_dir.join("traefik/dynamic/denia.yml")
        );
    }

    #[test]
    fn defaults_for_ports_and_image() {
        let c = base();
        assert_eq!(c.http_port, 80);
        assert_eq!(c.https_port, 443);
        assert!(c.traefik_image.starts_with("docker.io/library/traefik:"));
        assert!(c.acme_email.is_none());
    }

    #[test]
    fn require_acme_email_errors_when_tls_used_without_email() {
        let c = base();
        assert!(matches!(
            c.require_acme_email(true),
            Err(ConfigError::AcmeEmailRequired)
        ));
    }

    #[test]
    fn require_acme_email_ok_when_no_tls() {
        let c = base();
        assert!(c.require_acme_email(false).is_ok());
    }

    #[test]
    fn require_acme_email_ok_when_email_present() {
        let mut c = base();
        c.acme_email = Some("ops@example.com".into());
        assert!(c.require_acme_email(true).is_ok());
    }
}
