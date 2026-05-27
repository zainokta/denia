use std::{env, net::SocketAddr, path::PathBuf};

use rand::RngExt;
use sha2::Digest;
use thiserror::Error;

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use sha2::Sha256;
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hash: [u8; 32] = Sha256::digest(key).into();
        key_block[..32].copy_from_slice(&hash);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

pub fn compute_admin_token_hash(token: &str, key: &[u8]) -> String {
    hex::encode(hmac_sha256(key, token.as_bytes()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub admin_token_hash: String,
    pub admin_token_hmac_key: [u8; 32],
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
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DENIA_ADMIN_TOKEN must be set")]
    MissingAdminToken,
    #[error("DENIA_ADMIN_TOKEN must be at least 64 characters long")]
    AdminTokenTooShort,
    #[error("invalid DENIA_BIND_ADDR: {0}")]
    InvalidBindAddr(#[from] std::net::AddrParseError),
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let admin_token =
            env::var("DENIA_ADMIN_TOKEN").map_err(|_| ConfigError::MissingAdminToken)?;
        if admin_token.len() < 64 {
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
        let acme_resolver = env::var("DENIA_ACME_RESOLVER").unwrap_or_else(|_| "le".to_string());
        let control_domain = env::var("DENIA_CONTROL_DOMAIN").ok();
        let control_tls = env::var("DENIA_CONTROL_TLS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let node_disk_path = env::var("DENIA_NODE_DISK_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.clone());

        let mut admin_token_hmac_key = [0u8; 32];
        rand::rng().fill(&mut admin_token_hmac_key);
        let admin_token_hash = compute_admin_token_hash(&admin_token, &admin_token_hmac_key);

        Ok(Self {
            bind_addr,
            admin_token_hash,
            admin_token_hmac_key,
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
        })
    }

    pub fn for_test(admin_token: impl Into<String>) -> Self {
        let data_dir = PathBuf::from("/tmp/denia-test");
        let admin_token = admin_token.into();
        let mut admin_token_hmac_key = [0u8; 32];
        rand::rng().fill(&mut admin_token_hmac_key);
        let admin_token_hash = compute_admin_token_hash(&admin_token, &admin_token_hmac_key);
        Self {
            bind_addr: "127.0.0.1:0".parse().expect("valid test bind addr"),
            admin_token_hash,
            admin_token_hmac_key,
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
            traefik_dynamic_config_path: PathBuf::from("/tmp/denia-traefik.yml"),
            userns_base: 100000,
            userns_size: 65536,
            acme_resolver: "le".to_string(),
            control_domain: None,
            control_tls: false,
            node_disk_path: data_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_produces_correct_length() {
        let key = b"test-key";
        let data = b"test-data";
        let result = hmac_sha256(key, data);
        assert_eq!(result.len(), 32);
    }

    #[test]
    fn hmac_sha256_is_deterministic() {
        let key = b"secret-key-12345678901234567890";
        let data = b"hello world";
        let r1 = hmac_sha256(key, data);
        let r2 = hmac_sha256(key, data);
        assert_eq!(r1, r2);
    }

    #[test]
    fn hmac_sha256_different_keys_differ() {
        let data = b"same-data";
        let r1 = hmac_sha256(b"key-a", data);
        let r2 = hmac_sha256(b"key-b", data);
        assert_ne!(r1, r2);
    }

    #[test]
    fn hmac_sha256_different_data_differ() {
        let key = b"same-key";
        let r1 = hmac_sha256(key, b"data-a");
        let r2 = hmac_sha256(key, b"data-b");
        assert_ne!(r1, r2);
    }

    #[test]
    fn compute_admin_token_hash_is_deterministic() {
        let key = b"test-key-0000000000000000000000";
        let h1 = compute_admin_token_hash("my-token", key);
        let h2 = compute_admin_token_hash("my-token", key);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn compute_admin_token_hash_differs_per_key() {
        let h1 = compute_admin_token_hash("token", b"key-a-0000000000000000000000000");
        let h2 = compute_admin_token_hash("token", b"key-b-0000000000000000000000000");
        assert_ne!(h1, h2);
    }

    #[test]
    fn for_test_does_not_store_plaintext_token() {
        let config = AppConfig::for_test(
            "test-token-that-is-at-least-64-characters-long-for-testing-purposes",
        );
        assert!(config.admin_token_hash.len() == 64);
        assert!(!config.admin_token_hash.contains("test-token"));
    }
}
