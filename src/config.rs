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
    pub git_binary: PathBuf,
    pub sops_binary: PathBuf,
    pub socket_proxy_binary: PathBuf,
    pub runtime_dir: PathBuf,
    pub cgroup_root: PathBuf,
    pub artifact_dir: PathBuf,
    pub log_dir: PathBuf,
    pub userns_base: u32,
    pub userns_size: u32,
    pub control_domain: Option<String>,
    pub control_tls: bool,
    pub node_disk_path: PathBuf,
    pub acme_email: Option<String>,
    pub http_port: u16,
    pub https_port: u16,
    pub autoscale_interval_s: u64,
    pub autoscale_headroom_cpu_millis: u32,
    pub autoscale_headroom_mem_bytes: u64,
    /// ACME directory URL (Let's Encrypt production by default; staging/pebble
    /// via `DENIA_ACME_DIRECTORY_URL` for tests).
    pub acme_directory_url: String,
    /// Directory holding the ACME account key and per-domain cert material
    /// (`<tls_dir>/account.key`, `<tls_dir>/<domain>/{fullchain,key}.pem`).
    pub tls_dir: PathBuf,
    /// Persistent OCI layer cache root. Content-addressed: blobs filed under
    /// `<oci_cache_dir>/blobs/<algorithm>/<digest_hex>` with a sibling
    /// `<digest_hex>.lastref` mtime sidecar (ADR-022).
    pub oci_cache_dir: PathBuf,
    /// Verification mode for cache hits: `none` (path-only), `size`
    /// (default; matches `OciDescriptor.size`), or `full` (re-hashes).
    pub oci_cache_verify_on_hit: OciCacheVerifyMode,
    /// Garbage collection scan interval. Default = 7 days.
    pub oci_gc_interval_secs: u64,
    /// Retention threshold: blobs with `.lastref` mtime older than this and
    /// no live reference are eligible for deletion. Default = 7 days.
    pub oci_gc_retention_secs: u64,
    /// Age public key used to encrypt control-plane-managed secrets (registry
    /// credentials, etc.). Required at the point of first encryption; absence
    /// is reported as a 400/500 at API time, not at boot. See ADR-021.
    pub age_recipient: Option<String>,
}

/// How aggressively a cache hit is re-verified before reuse (ADR-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciCacheVerifyMode {
    /// Trust the on-disk file as-is (cheapest; assume the cache is local and
    /// not externally modified). Path existence is still required.
    None,
    /// Confirm the on-disk size matches the descriptor's declared size.
    Size,
    /// Re-stream the file through SHA-256 and confirm the digest matches the
    /// descriptor. Catches silent on-disk corruption.
    Full,
}

impl OciCacheVerifyMode {
    pub fn parse_env(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "size" => Some(Self::Size),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// Default ACME directory: Let's Encrypt production.
pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DENIA_ADMIN_TOKEN must be set")]
    MissingAdminToken,
    #[error("DENIA_ADMIN_TOKEN must be at least 64 characters long")]
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
        let git_binary =
            PathBuf::from(env::var("DENIA_GIT_BINARY").unwrap_or_else(|_| "git".to_string()));
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
        let acme_email = env::var("DENIA_ACME_EMAIL").ok().filter(|v| !v.is_empty());
        let http_port = env::var("DENIA_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80);
        let https_port = env::var("DENIA_HTTPS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(443);
        let userns_base = env::var("DENIA_USERNS_BASE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100000);
        let userns_size = env::var("DENIA_USERNS_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(65536);
        let control_domain = env::var("DENIA_CONTROL_DOMAIN").ok();
        let control_tls = env::var("DENIA_CONTROL_TLS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        let node_disk_path = env::var("DENIA_NODE_DISK_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.clone());
        let autoscale_interval_s = env::var("DENIA_AUTOSCALE_INTERVAL_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15);
        let autoscale_headroom_cpu_millis = env::var("DENIA_AUTOSCALE_HEADROOM_CPU_MILLIS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let autoscale_headroom_mem_bytes = env::var("DENIA_AUTOSCALE_HEADROOM_MEM_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(536870912);
        let acme_directory_url = env::var("DENIA_ACME_DIRECTORY_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_ACME_DIRECTORY_URL.to_string());
        let tls_dir = env::var("DENIA_TLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("tls"));
        let oci_cache_dir = env::var("DENIA_OCI_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("oci-cache"));
        let oci_cache_verify_on_hit = env::var("DENIA_OCI_CACHE_VERIFY_ON_HIT")
            .ok()
            .and_then(|v| OciCacheVerifyMode::parse_env(&v))
            .unwrap_or(OciCacheVerifyMode::Size);
        let oci_gc_interval_secs = env::var("DENIA_OCI_GC_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7 * 24 * 60 * 60);
        let oci_gc_retention_secs = env::var("DENIA_OCI_GC_RETENTION_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7 * 24 * 60 * 60);
        let age_recipient = env::var("DENIA_AGE_RECIPIENT")
            .ok()
            .filter(|v| !v.trim().is_empty());

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
            git_binary,
            sops_binary,
            socket_proxy_binary,
            runtime_dir,
            cgroup_root,
            artifact_dir,
            log_dir,
            userns_base,
            userns_size,
            control_domain,
            control_tls,
            node_disk_path,
            acme_email,
            http_port,
            https_port,
            autoscale_interval_s,
            autoscale_headroom_cpu_millis,
            autoscale_headroom_mem_bytes,
            acme_directory_url,
            tls_dir,
            oci_cache_dir,
            oci_cache_verify_on_hit,
            oci_gc_interval_secs,
            oci_gc_retention_secs,
            age_recipient,
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
            git_binary: PathBuf::from("git"),
            sops_binary: PathBuf::from("sops"),
            socket_proxy_binary: PathBuf::from("denia"),
            runtime_dir: data_dir.join("runtime"),
            cgroup_root: data_dir.join("cgroup"),
            artifact_dir: data_dir.join("artifacts"),
            log_dir: data_dir.join("logs"),
            userns_base: 100000,
            userns_size: 65536,
            control_domain: None,
            control_tls: false,
            node_disk_path: data_dir.clone(),
            acme_email: None,
            http_port: 80,
            https_port: 443,
            autoscale_interval_s: 15,
            autoscale_headroom_cpu_millis: 1000,
            autoscale_headroom_mem_bytes: 536870912,
            acme_directory_url: DEFAULT_ACME_DIRECTORY_URL.to_string(),
            tls_dir: data_dir.join("tls"),
            oci_cache_dir: data_dir.join("oci-cache"),
            oci_cache_verify_on_hit: OciCacheVerifyMode::Size,
            // Tests do not rely on the GC interval directly; the loop runs
            // a `sweep_once` per tick and any test that needs determinism
            // calls `sweep_once` synchronously.
            oci_gc_interval_secs: 7 * 24 * 60 * 60,
            oci_gc_retention_secs: 7 * 24 * 60 * 60,
            age_recipient: Some("age1test".into()),
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
mod ingress_tls_tests {
    use super::*;

    fn base() -> AppConfig {
        AppConfig::for_test("0123456789012345678901234567890123")
    }

    #[test]
    fn defaults_for_ports() {
        let c = base();
        assert_eq!(c.http_port, 80);
        assert_eq!(c.https_port, 443);
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

    #[test]
    fn tls_dir_defaults_under_data_dir() {
        let c = base();
        assert_eq!(c.tls_dir, c.data_dir.join("tls"));
    }

    #[test]
    fn acme_directory_defaults_to_lets_encrypt_prod() {
        let c = base();
        assert_eq!(c.acme_directory_url, DEFAULT_ACME_DIRECTORY_URL);
        assert!(c.acme_directory_url.starts_with("https://"));
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

    // Both presence and absence are asserted in one test because cargo runs
    // unit tests in parallel and DENIA_AGE_RECIPIENT is process-global env.
    #[test]
    fn age_recipient_env_round_trip() {
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));

        // Absent.
        unsafe {
            std::env::remove_var("DENIA_AGE_RECIPIENT");
        }
        let cfg = AppConfig::from_env().expect("config from env");
        assert!(cfg.age_recipient.is_none());

        // Present.
        let _recipient = EnvGuard::set("DENIA_AGE_RECIPIENT", "age1qy0testrecipient");
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.age_recipient.as_deref(), Some("age1qy0testrecipient"));

        // Empty/whitespace treated as absent.
        let _empty = EnvGuard::set("DENIA_AGE_RECIPIENT", "   ");
        let cfg = AppConfig::from_env().expect("config from env");
        assert!(cfg.age_recipient.is_none());
    }

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: impl AsRef<str>) -> Self {
            let prior = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, val.as_ref());
            }
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe {
                    std::env::set_var(self.key, v);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }
}
