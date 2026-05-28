use std::{
    env,
    io::Write,
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use rand::RngExt;
use serde::{Deserialize, Serialize};
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

/// Default location for the Denia-owned age private key. The control plane
/// derives the encryption recipient from this file unless `DENIA_AGE_RECIPIENT`
/// is set explicitly. See ADR-021.
fn default_age_key_path() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
        .join(".config/denia/age.key")
}

/// Extract the `age1...` public key from an age private-key file by scanning
/// for the `# public key:` header comment that `age-keygen` writes. Returns
/// `None` if the file is missing or the comment is absent.
fn read_age_public_key(path: &std::path::Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("# public key:") {
            let key = rest.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// TOML-backed configuration. Every field is optional: env vars override file
/// values, file values fill in for unset env vars, and missing fields fall
/// back to the hardcoded defaults below. The default location is
/// `$XDG_CONFIG_HOME/denia/config.toml` (or `$HOME/.config/denia/config.toml`
/// when `XDG_CONFIG_HOME` is unset). Override the path with
/// `DENIA_CONFIG_FILE` for tests or alternative deployments.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub admin_token: Option<String>,
    pub bind_addr: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub database_path: Option<PathBuf>,
    pub buildkit_binary: Option<PathBuf>,
    pub git_binary: Option<PathBuf>,
    pub sops_binary: Option<PathBuf>,
    pub socket_proxy_binary: Option<PathBuf>,
    pub cgroup_root: Option<PathBuf>,
    pub acme_email: Option<String>,
    pub http_port: Option<u16>,
    pub https_port: Option<u16>,
    pub userns_base: Option<u32>,
    pub userns_size: Option<u32>,
    pub control_domain: Option<String>,
    pub control_tls: Option<bool>,
    pub node_disk_path: Option<PathBuf>,
    pub autoscale_interval_s: Option<u64>,
    pub autoscale_headroom_cpu_millis: Option<u32>,
    pub autoscale_headroom_mem_bytes: Option<u64>,
    pub acme_directory_url: Option<String>,
    pub tls_dir: Option<PathBuf>,
    pub oci_cache_dir: Option<PathBuf>,
    pub oci_cache_verify_on_hit: Option<String>,
    pub oci_gc_interval_secs: Option<u64>,
    pub oci_gc_retention_secs: Option<u64>,
    pub age_recipient: Option<String>,
    pub age_key_file: Option<PathBuf>,
}

/// Resolve the on-disk config file path.
pub fn config_file_path() -> PathBuf {
    if let Some(p) = env::var_os("DENIA_CONFIG_FILE") {
        return PathBuf::from(p);
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/root/.config"));
    base.join("denia").join("config.toml")
}

/// Build a fully-populated default `FileConfig` template. Used when the
/// config file is missing so the operator sees every tunable on first run.
/// `admin_token` is freshly generated (32 random bytes -> 64 hex chars).
fn default_file_template() -> FileConfig {
    let mut admin_token_bytes = [0u8; 32];
    rand::rng().fill(&mut admin_token_bytes);
    FileConfig {
        admin_token: Some(hex::encode(admin_token_bytes)),
        bind_addr: Some("127.0.0.1:7180".to_string()),
        data_dir: Some(PathBuf::from("/var/lib/denia")),
        database_path: None,
        buildkit_binary: Some(PathBuf::from("buildctl")),
        git_binary: Some(PathBuf::from("git")),
        sops_binary: Some(PathBuf::from("sops")),
        socket_proxy_binary: None,
        cgroup_root: Some(PathBuf::from("/sys/fs/cgroup/denia")),
        acme_email: None,
        http_port: Some(80),
        https_port: Some(443),
        userns_base: Some(100_000),
        userns_size: Some(65_536),
        control_domain: None,
        control_tls: Some(false),
        node_disk_path: None,
        autoscale_interval_s: Some(15),
        autoscale_headroom_cpu_millis: Some(1_000),
        autoscale_headroom_mem_bytes: Some(536_870_912),
        acme_directory_url: Some(DEFAULT_ACME_DIRECTORY_URL.to_string()),
        tls_dir: None,
        oci_cache_dir: None,
        oci_cache_verify_on_hit: Some("size".to_string()),
        oci_gc_interval_secs: Some(7 * 24 * 60 * 60),
        oci_gc_retention_secs: Some(7 * 24 * 60 * 60),
        age_recipient: None,
        age_key_file: None,
    }
}

/// Header written above every newly generated config file. Comments are not
/// preserved on rewrite (we never rewrite this file from code), but the
/// header is helpful on first inspection.
const CONFIG_FILE_HEADER: &str = "\
# Denia control-plane configuration.
#
# This file was auto-generated on first run. Environment variables override
# any value here (e.g. DENIA_ADMIN_TOKEN, DENIA_BIND_ADDR). Unset fields fall
# back to the documented hardcoded defaults.
#
# admin_token: 64-char hex string. Keep secret. Regenerate by deleting this
# file and restarting the daemon.

";

/// Read the TOML config from disk, or generate + persist a default template
/// when the file does not exist. The file is created `0600`.
fn load_or_create_file_config() -> Result<FileConfig, ConfigError> {
    let path = config_file_path();
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::ConfigFileIo(path.clone(), e))?;
        let cfg: FileConfig =
            toml::from_str(&raw).map_err(|e| ConfigError::ConfigFileParse(path.clone(), e))?;
        return Ok(cfg);
    }
    let default = default_file_template();
    write_default_config_file(&path, &default)?;
    Ok(default)
}

fn write_default_config_file(path: &Path, cfg: &FileConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ConfigError::ConfigFileIo(parent.to_path_buf(), e))?;
    }
    let serialized =
        toml::to_string_pretty(cfg).map_err(|e| ConfigError::ConfigFileSerialize(e.to_string()))?;
    let body = format!("{CONFIG_FILE_HEADER}{serialized}");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| ConfigError::ConfigFileIo(path.to_path_buf(), e))?;
    f.write_all(body.as_bytes())
        .map_err(|e| ConfigError::ConfigFileIo(path.to_path_buf(), e))?;
    Ok(())
}

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
    #[error("config file I/O error at {0}: {1}")]
    ConfigFileIo(PathBuf, #[source] std::io::Error),
    #[error("config file parse error at {0}: {1}")]
    ConfigFileParse(PathBuf, #[source] toml::de::Error),
    #[error("config file serialize error: {0}")]
    ConfigFileSerialize(String),
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let file_cfg = load_or_create_file_config()?;

        let admin_token = env::var("DENIA_ADMIN_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| file_cfg.admin_token.clone().filter(|v| !v.is_empty()))
            .ok_or(ConfigError::MissingAdminToken)?;
        if admin_token.len() < 64 {
            return Err(ConfigError::AdminTokenTooShort);
        }
        let bind_addr: SocketAddr = env::var("DENIA_BIND_ADDR")
            .ok()
            .or_else(|| file_cfg.bind_addr.clone())
            .unwrap_or_else(|| "127.0.0.1:7180".to_string())
            .parse()?;
        let data_dir = env::var("DENIA_DATA_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.data_dir.clone())
            .unwrap_or_else(|| PathBuf::from("/var/lib/denia"));
        let database_path = env::var("DENIA_DATABASE_PATH")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.database_path.clone())
            .unwrap_or_else(|| data_dir.join("denia.sqlite3"));
        let buildkit_binary = env::var("DENIA_BUILDKIT_BINARY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.buildkit_binary.clone())
            .unwrap_or_else(|| PathBuf::from("buildctl"));
        let git_binary = env::var("DENIA_GIT_BINARY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.git_binary.clone())
            .unwrap_or_else(|| PathBuf::from("git"));
        let sops_binary = env::var("DENIA_SOPS_BINARY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.sops_binary.clone())
            .unwrap_or_else(|| PathBuf::from("sops"));
        let socket_proxy_binary = env::var("DENIA_SOCKET_PROXY_BINARY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.socket_proxy_binary.clone())
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "denia".into()));
        let runtime_dir = data_dir.join("runtime");
        let cgroup_root = env::var("DENIA_CGROUP_ROOT")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.cgroup_root.clone())
            .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup/denia"));
        let artifact_dir = data_dir.join("artifacts");
        let log_dir = data_dir.join("logs");
        let acme_email = env::var("DENIA_ACME_EMAIL")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| file_cfg.acme_email.clone().filter(|v| !v.is_empty()));
        let http_port = env::var("DENIA_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.http_port)
            .unwrap_or(80);
        let https_port = env::var("DENIA_HTTPS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.https_port)
            .unwrap_or(443);
        let userns_base = env::var("DENIA_USERNS_BASE")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.userns_base)
            .unwrap_or(100_000);
        let userns_size = env::var("DENIA_USERNS_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.userns_size)
            .unwrap_or(65_536);
        let control_domain = env::var("DENIA_CONTROL_DOMAIN")
            .ok()
            .or_else(|| file_cfg.control_domain.clone());
        let control_tls = env::var("DENIA_CONTROL_TLS")
            .ok()
            .map(|v| v == "1" || v == "true")
            .or(file_cfg.control_tls)
            .unwrap_or(false);
        let node_disk_path = env::var("DENIA_NODE_DISK_PATH")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.node_disk_path.clone())
            .unwrap_or_else(|| data_dir.clone());
        let autoscale_interval_s = env::var("DENIA_AUTOSCALE_INTERVAL_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.autoscale_interval_s)
            .unwrap_or(15);
        let autoscale_headroom_cpu_millis = env::var("DENIA_AUTOSCALE_HEADROOM_CPU_MILLIS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.autoscale_headroom_cpu_millis)
            .unwrap_or(1_000);
        let autoscale_headroom_mem_bytes = env::var("DENIA_AUTOSCALE_HEADROOM_MEM_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.autoscale_headroom_mem_bytes)
            .unwrap_or(536_870_912);
        let acme_directory_url = env::var("DENIA_ACME_DIRECTORY_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                file_cfg
                    .acme_directory_url
                    .clone()
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| DEFAULT_ACME_DIRECTORY_URL.to_string());
        let tls_dir = env::var("DENIA_TLS_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.tls_dir.clone())
            .unwrap_or_else(|| data_dir.join("tls"));
        let oci_cache_dir = env::var("DENIA_OCI_CACHE_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.oci_cache_dir.clone())
            .unwrap_or_else(|| data_dir.join("oci-cache"));
        let oci_cache_verify_on_hit = env::var("DENIA_OCI_CACHE_VERIFY_ON_HIT")
            .ok()
            .and_then(|v| OciCacheVerifyMode::parse_env(&v))
            .or_else(|| {
                file_cfg
                    .oci_cache_verify_on_hit
                    .as_deref()
                    .and_then(OciCacheVerifyMode::parse_env)
            })
            .unwrap_or(OciCacheVerifyMode::Size);
        let oci_gc_interval_secs = env::var("DENIA_OCI_GC_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.oci_gc_interval_secs)
            .unwrap_or(7 * 24 * 60 * 60);
        let oci_gc_retention_secs = env::var("DENIA_OCI_GC_RETENTION_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.oci_gc_retention_secs)
            .unwrap_or(7 * 24 * 60 * 60);
        let age_recipient = env::var("DENIA_AGE_RECIPIENT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                file_cfg
                    .age_recipient
                    .clone()
                    .filter(|v| !v.trim().is_empty())
            })
            .or_else(|| {
                let key_path = env::var("DENIA_AGE_KEY_FILE")
                    .ok()
                    .map(PathBuf::from)
                    .or_else(|| file_cfg.age_key_file.clone())
                    .unwrap_or_else(default_age_key_path);
                read_age_public_key(&key_path)
            });

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

    // Serializes any test that calls `AppConfig::from_env`; that function
    // mutates process-global env vars *and* touches a host-shared default
    // config path, so two such tests cannot run concurrently.
    static FROM_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point `DENIA_CONFIG_FILE` at a per-test tempfile so `from_env` does not
    /// touch `~/.config/denia/config.toml`. Returned guards must outlive the
    /// `from_env` calls.
    fn isolated_config_file() -> (tempfile::TempDir, EnvGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let guard = EnvGuard::set("DENIA_CONFIG_FILE", path.to_string_lossy().as_ref());
        (dir, guard)
    }

    // Both presence and absence are asserted in one test because cargo runs
    // unit tests in parallel and DENIA_AGE_RECIPIENT is process-global env.
    #[test]
    fn age_recipient_env_round_trip() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        // Pin DENIA_AGE_KEY_FILE to a missing path so the auto-detect fallback
        // does not pick up the developer's real ~/.config/denia/age.key.
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");

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

        // Auto-detect: empty DENIA_AGE_RECIPIENT falls back to DENIA_AGE_KEY_FILE.
        let auto_dir = tempfile::tempdir().expect("tempdir");
        let auto_key = auto_dir.path().join("age.key");
        std::fs::write(
            &auto_key,
            "# public key: age1autodetectkey\nAGE-SECRET-KEY-1AAA\n",
        )
        .unwrap();
        let _auto_key_file =
            EnvGuard::set("DENIA_AGE_KEY_FILE", auto_key.to_string_lossy().as_ref());
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.age_recipient.as_deref(), Some("age1autodetectkey"));

        // Explicit DENIA_AGE_RECIPIENT wins over the key file.
        let _explicit = EnvGuard::set("DENIA_AGE_RECIPIENT", "age1explicitwins");
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.age_recipient.as_deref(), Some("age1explicitwins"));
    }

    #[test]
    fn from_env_creates_default_config_file_when_missing() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let _cfg_file = EnvGuard::set("DENIA_CONFIG_FILE", path.to_string_lossy().as_ref());
        // Force the file to provide admin_token; remove the env var.
        unsafe {
            std::env::remove_var("DENIA_ADMIN_TOKEN");
        }
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");

        assert!(!path.exists());
        let cfg = AppConfig::from_env().expect("config from env");
        assert!(path.exists(), "config file should be created");

        // Generated admin_token is 64 hex chars and survives into AppConfig.
        let raw = std::fs::read_to_string(&path).expect("read created config");
        let file_cfg: FileConfig = toml::from_str(&raw).expect("parse created config");
        let token = file_cfg.admin_token.expect("token written");
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // The hash in AppConfig is derived from this token + a random HMAC key,
        // so we cannot equality-check; just confirm the hash has the right shape.
        assert_eq!(cfg.admin_token_hash.len(), 64);

        // 0600 perms.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config file must be 0600");
    }

    #[test]
    fn env_overrides_file_value() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
admin_token = "file-token-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
http_port = 8080
control_tls = true
"#,
        )
        .unwrap();
        let _cfg_file = EnvGuard::set("DENIA_CONFIG_FILE", path.to_string_lossy().as_ref());
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        unsafe {
            std::env::remove_var("DENIA_ADMIN_TOKEN");
            std::env::remove_var("DENIA_HTTP_PORT");
            std::env::remove_var("DENIA_CONTROL_TLS");
            std::env::remove_var("DENIA_AGE_RECIPIENT");
        }

        // File values used when env unset.
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.http_port, 8080);
        assert!(cfg.control_tls);

        // Env wins when set.
        let _http = EnvGuard::set("DENIA_HTTP_PORT", "9090");
        let _tls = EnvGuard::set("DENIA_CONTROL_TLS", "false");
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.http_port, 9090);
        assert!(!cfg.control_tls);
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

    #[test]
    fn read_age_public_key_parses_keygen_comment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("age.key");
        std::fs::write(
            &path,
            "# created: 2026-05-28T00:00:00Z\n\
             # public key: age1exampleabcdef\n\
             AGE-SECRET-KEY-1QQQQQQ\n",
        )
        .unwrap();
        assert_eq!(
            read_age_public_key(&path).as_deref(),
            Some("age1exampleabcdef"),
        );
    }

    #[test]
    fn read_age_public_key_missing_file_returns_none() {
        let path = std::path::Path::new("/nonexistent/denia-test/age.key");
        assert!(read_age_public_key(path).is_none());
    }

    #[test]
    fn read_age_public_key_without_comment_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("age.key");
        std::fs::write(&path, "AGE-SECRET-KEY-1QQQQQQ\n").unwrap();
        assert!(read_age_public_key(&path).is_none());
    }
}
