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

use crate::ingress::l4::PortRange;

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
    pub tcp_port_range: PortRange,
    pub udp_port_range: PortRange,
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
    /// Hosted registry garbage-collection scan interval. Default = 24 hours.
    pub registry_gc_interval_secs: u64,
    /// Hosted registry grace period: unreferenced blobs younger than this are
    /// not deleted (guards against an in-flight push between blob upload and
    /// manifest commit). Default = 1 hour.
    pub registry_gc_grace_secs: u64,
    /// Maximum on-the-wire size accepted for a single hosted-registry blob
    /// upload (the cumulative size of all PATCH chunks plus the trailing PUT
    /// body). Enforced while streaming to disk so an Operator-capable token
    /// cannot OOM the daemon. Default = 10 GiB. Override with
    /// `DENIA_REGISTRY_MAX_BLOB_BYTES`.
    pub registry_max_blob_bytes: u64,
    /// Maximum size accepted for a single hosted-registry manifest body.
    /// Manifests are small JSON documents; this is a generous bound that still
    /// rejects pathological payloads. Default = 16 MiB. Override with
    /// `DENIA_REGISTRY_MAX_MANIFEST_BYTES`.
    pub registry_max_manifest_bytes: u64,
    /// Age public key used to encrypt control-plane-managed secrets (registry
    /// credentials, etc.). Required at the point of first encryption; absence
    /// is reported as a 400/500 at API time, not at boot. See ADR-021.
    pub age_recipient: Option<String>,
    /// Age private-key file the daemon passes to `sops` as `SOPS_AGE_KEY_FILE`
    /// when decrypting secrets at deploy time. Resolved from `DENIA_AGE_KEY_FILE`
    /// / config `age_key_file` / the operator-home default. See ADR-021/ADR-023.
    pub age_key_file: PathBuf,
    /// Staging area for push-upload tarballs before extraction. Defaults to
    /// `<data_dir>/uploads`. See ADR-034.
    pub uploads_dir: PathBuf,
    /// Maximum compressed (on-the-wire) body size accepted for a push upload.
    /// Default = 512 MiB. Override with `DENIA_UPLOAD_MAX_BYTES`.
    pub upload_max_bytes: u64,
    /// Maximum uncompressed size of the extracted tarball. Default = 2 GiB.
    /// Override with `DENIA_UPLOAD_MAX_UNCOMPRESSED_BYTES`.
    pub upload_max_uncompressed_bytes: u64,
    /// Maximum number of tar entries allowed in a single upload archive.
    /// Default = 200 000. Override with `DENIA_UPLOAD_MAX_ENTRIES`.
    pub upload_max_entries: u64,
    /// Seconds before a staged upload is eligible for garbage collection.
    /// Default = 3600 (1 hour). Override with `DENIA_UPLOAD_TTL_SECS`.
    pub upload_ttl_secs: u64,
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

/// Operator `.config` directory resolved from `$SUDO_USER` (+ `getent passwd`),
/// so a manual `sudo ./denia` reads the same location `denia setup` wrote to.
/// Mirrors `cli::common::privilege::detect_install_user`. Returns `None` when
/// not invoked via sudo from a real account — e.g. the production daemon running
/// as the `denia` system user, or a normal foreground run.
fn operator_config_base() -> Option<PathBuf> {
    let user = env::var("SUDO_USER")
        .ok()
        .filter(|u| !u.is_empty() && u != "root")?;
    let out = std::process::Command::new("getent")
        .args(["passwd", &user])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8(out.stdout).ok()?;
    let home = line.trim_end_matches('\n').split(':').nth(5)?.trim();
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".config"))
}

/// The `<base>/denia` config directory the daemon reads operator state from.
/// Precedence: operator home (`$SUDO_USER`) → `XDG_CONFIG_HOME` →
/// `$HOME/.config` → `/root/.config`. Explicit `DENIA_CONFIG_FILE` /
/// `DENIA_AGE_KEY_FILE` still take precedence at their call sites.
fn config_dir() -> PathBuf {
    operator_config_base()
        .or_else(|| env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("/root/.config"))
        .join("denia")
}

/// Default location for the Denia-owned age private key. The control plane
/// derives the encryption recipient from this file unless `DENIA_AGE_RECIPIENT`
/// is set explicitly. See ADR-021.
fn default_age_key_path() -> PathBuf {
    config_dir().join("age.key")
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
/// back to the hardcoded defaults below. The default location is resolved by
/// [`config_file_path`] (operator home via `$SUDO_USER`, then `XDG_CONFIG_HOME`,
/// then `$HOME/.config`). Override the path with the `--config` flag or
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
    pub tcp_port_range: Option<String>,
    pub udp_port_range: Option<String>,
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
    pub uploads_dir: Option<PathBuf>,
    pub upload_max_bytes: Option<u64>,
    pub upload_max_uncompressed_bytes: Option<u64>,
    pub upload_max_entries: Option<u64>,
    pub upload_ttl_secs: Option<u64>,
}

/// Resolve the on-disk config file path. `DENIA_CONFIG_FILE` (and thus the
/// `--config` flag) wins; otherwise it falls under [`config_dir`].
pub fn config_file_path() -> PathBuf {
    if let Some(p) = env::var_os("DENIA_CONFIG_FILE") {
        return PathBuf::from(p);
    }
    config_dir().join("config.toml")
}

/// Fallback admin-token source for manual (non-systemd) runs: read the
/// `admin.token` env-file `denia setup` writes next to `config.toml`. In
/// production the systemd unit loads it via `EnvironmentFile`; this lets a
/// direct `./denia` run pick it up from the same operator config dir. Accepts
/// either the `DENIA_ADMIN_TOKEN=<value>` env-file line or a bare token. The
/// token value is never logged.
fn read_admin_token_file() -> Option<String> {
    let token_path = config_file_path().parent()?.join("admin.token");
    let contents = std::fs::read_to_string(&token_path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("DENIA_ADMIN_TOKEN=") {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        } else if !line.contains('=') {
            let value = line.trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
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
        tcp_port_range: Some(DEFAULT_TCP_PORT_RANGE.to_string()),
        udp_port_range: Some(DEFAULT_UDP_PORT_RANGE.to_string()),
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
        uploads_dir: None,
        upload_max_bytes: Some(536_870_912),
        upload_max_uncompressed_bytes: Some(2_147_483_648),
        upload_max_entries: Some(200_000),
        upload_ttl_secs: Some(3_600),
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
    #[error("invalid DENIA_CONTROL_DOMAIN: {0}")]
    InvalidControlDomain(String),
    #[error("invalid {name}: {value}")]
    InvalidPortRange { name: &'static str, value: String },
}

pub const DEFAULT_TCP_PORT_RANGE: &str = "20000-29999";
pub const DEFAULT_UDP_PORT_RANGE: &str = "30000-39999";

fn parse_port_range_config(
    name: &'static str,
    value: Option<String>,
    default: &'static str,
) -> Result<PortRange, ConfigError> {
    let raw = value.unwrap_or_else(|| default.to_string());
    PortRange::parse(&raw).map_err(|_| ConfigError::InvalidPortRange { name, value: raw })
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let file_cfg = load_or_create_file_config()?;

        let admin_token = env::var("DENIA_ADMIN_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| file_cfg.admin_token.clone().filter(|v| !v.is_empty()))
            .or_else(read_admin_token_file)
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
        // Default DB location matches the provisioned layout (`denia setup`
        // creates `<data_dir>/sqlite/` and the rendered config.toml points
        // here). An env-only / `cargo run` deploy now lands in the same place
        // as a setup install; `SqliteStore::open` creates the parent dir.
        let database_path = env::var("DENIA_DATABASE_PATH")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.database_path.clone())
            .unwrap_or_else(|| data_dir.join("sqlite").join("denia.sqlite3"));
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
        // Defaults to this running binary so the daemon re-execs itself as the
        // socket proxy. If `current_exe()` fails (rare; e.g. the exe was
        // unlinked), fall back to the documented absolute install path rather
        // than a bare relative `denia`, which a hardened unit's reduced PATH
        // may not resolve.
        let socket_proxy_binary = env::var("DENIA_SOCKET_PROXY_BINARY")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.socket_proxy_binary.clone())
            .unwrap_or_else(|| {
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/local/bin/denia"))
            });
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
        let tcp_port_range = parse_port_range_config(
            "DENIA_TCP_PORT_RANGE",
            env::var("DENIA_TCP_PORT_RANGE")
                .ok()
                .or_else(|| file_cfg.tcp_port_range.clone()),
            DEFAULT_TCP_PORT_RANGE,
        )?;
        let udp_port_range = parse_port_range_config(
            "DENIA_UDP_PORT_RANGE",
            env::var("DENIA_UDP_PORT_RANGE")
                .ok()
                .or_else(|| file_cfg.udp_port_range.clone()),
            DEFAULT_UDP_PORT_RANGE,
        )?;
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
            .or_else(|| file_cfg.control_domain.clone())
            .filter(|v| !v.trim().is_empty())
            .map(|d| {
                crate::ingress::pingora::state::validate_domain(&d)
                    .map_err(|e| ConfigError::InvalidControlDomain(e.to_string()))
            })
            .transpose()?;
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
        let registry_gc_interval_secs = env::var("DENIA_REGISTRY_GC_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24 * 60 * 60);
        let registry_gc_grace_secs = env::var("DENIA_REGISTRY_GC_GRACE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60 * 60);
        let registry_max_blob_bytes = env::var("DENIA_REGISTRY_MAX_BLOB_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10u64 * 1024 * 1024 * 1024);
        let registry_max_manifest_bytes = env::var("DENIA_REGISTRY_MAX_MANIFEST_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16u64 * 1024 * 1024);
        let age_key_file = env::var("DENIA_AGE_KEY_FILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| file_cfg.age_key_file.clone())
            .unwrap_or_else(default_age_key_path);
        let age_recipient = env::var("DENIA_AGE_RECIPIENT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                file_cfg
                    .age_recipient
                    .clone()
                    .filter(|v| !v.trim().is_empty())
            })
            .or_else(|| read_age_public_key(&age_key_file));
        // Aid diagnosis: an unparsable / comment-less key file silently yields
        // no recipient, which surfaces much later as a 400 on registry create.
        // Never log key contents — only the path.
        if age_recipient.is_none() && age_key_file.exists() {
            tracing::warn!(
                age_key_file = %age_key_file.display(),
                "age key file present but no recipient parsed (missing `# public key:` comment); \
                 set DENIA_AGE_RECIPIENT or age_recipient explicitly"
            );
        }
        let uploads_dir = env::var("DENIA_UPLOADS_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| file_cfg.uploads_dir.clone())
            .unwrap_or_else(|| data_dir.join("uploads"));
        let upload_max_bytes = env::var("DENIA_UPLOAD_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.upload_max_bytes)
            .unwrap_or(536_870_912);
        let upload_max_uncompressed_bytes = env::var("DENIA_UPLOAD_MAX_UNCOMPRESSED_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.upload_max_uncompressed_bytes)
            .unwrap_or(2_147_483_648);
        let upload_max_entries = env::var("DENIA_UPLOAD_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.upload_max_entries)
            .unwrap_or(200_000);
        let upload_ttl_secs = env::var("DENIA_UPLOAD_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.upload_ttl_secs)
            .unwrap_or(3_600);

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
            tcp_port_range,
            udp_port_range,
            autoscale_interval_s,
            autoscale_headroom_cpu_millis,
            autoscale_headroom_mem_bytes,
            acme_directory_url,
            tls_dir,
            oci_cache_dir,
            oci_cache_verify_on_hit,
            oci_gc_interval_secs,
            oci_gc_retention_secs,
            registry_gc_interval_secs,
            registry_gc_grace_secs,
            registry_max_blob_bytes,
            registry_max_manifest_bytes,
            age_recipient,
            age_key_file,
            uploads_dir,
            upload_max_bytes,
            upload_max_uncompressed_bytes,
            upload_max_entries,
            upload_ttl_secs,
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
            tcp_port_range: PortRange::parse(DEFAULT_TCP_PORT_RANGE).expect("default tcp range"),
            udp_port_range: PortRange::parse(DEFAULT_UDP_PORT_RANGE).expect("default udp range"),
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
            registry_gc_interval_secs: 24 * 60 * 60,
            registry_gc_grace_secs: 60 * 60,
            registry_max_blob_bytes: 10u64 * 1024 * 1024 * 1024,
            registry_max_manifest_bytes: 16u64 * 1024 * 1024,
            age_recipient: Some("age1test".into()),
            age_key_file: data_dir.join("age.key"),
            uploads_dir: data_dir.join("uploads"),
            upload_max_bytes: 536_870_912,
            upload_max_uncompressed_bytes: 2_147_483_648,
            upload_max_entries: 200_000,
            upload_ttl_secs: 3_600,
        }
    }

    pub fn require_acme_email(&self, tls_in_use: bool) -> Result<(), ConfigError> {
        let control_tls_in_use = self.control_tls && self.control_domain.is_some();
        if (tls_in_use || control_tls_in_use) && self.acme_email.is_none() {
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
        assert_eq!(c.tcp_port_range, PortRange::new(20000, 29999));
        assert_eq!(c.udp_port_range, PortRange::new(30000, 39999));
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
    fn require_acme_email_errors_when_control_domain_tls_without_email() {
        let mut c = base();
        c.control_domain = Some("denia.example.com".into());
        c.control_tls = true;
        assert!(matches!(
            c.require_acme_email(false),
            Err(ConfigError::AcmeEmailRequired)
        ));
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

    // Mirrors `denia setup`: a config.toml that omits admin_token, with the
    // token in a sibling admin.token env-file. A manual run (no systemd
    // EnvironmentFile, no DENIA_ADMIN_TOKEN) must still pick it up.
    #[test]
    fn admin_token_read_from_sibling_token_file() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (cfg_dir, _cfg_file) = isolated_config_file();
        std::fs::write(
            cfg_dir.path().join("config.toml"),
            "bind_addr = \"127.0.0.1:0\"\n",
        )
        .unwrap();
        unsafe {
            std::env::remove_var("DENIA_ADMIN_TOKEN");
        }
        let token = "a".repeat(64);
        std::fs::write(
            cfg_dir.path().join("admin.token"),
            format!("DENIA_ADMIN_TOKEN={token}\n"),
        )
        .unwrap();
        // Keep the recipient auto-detect inert.
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");

        let cfg = AppConfig::from_env().expect("admin token read from sibling admin.token");
        assert_eq!(cfg.admin_token_hash.len(), 64);
        assert!(!cfg.admin_token_hash.contains(token.as_str()));
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
        assert_eq!(
            file_cfg.tcp_port_range.as_deref(),
            Some(DEFAULT_TCP_PORT_RANGE)
        );
        assert_eq!(
            file_cfg.udp_port_range.as_deref(),
            Some(DEFAULT_UDP_PORT_RANGE)
        );
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
tcp_port_range = "21000-21002"
udp_port_range = "31000-31002"
"#,
        )
        .unwrap();
        let _cfg_file = EnvGuard::set("DENIA_CONFIG_FILE", path.to_string_lossy().as_ref());
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        unsafe {
            std::env::remove_var("DENIA_ADMIN_TOKEN");
            std::env::remove_var("DENIA_HTTP_PORT");
            std::env::remove_var("DENIA_CONTROL_TLS");
            std::env::remove_var("DENIA_TCP_PORT_RANGE");
            std::env::remove_var("DENIA_UDP_PORT_RANGE");
            std::env::remove_var("DENIA_AGE_RECIPIENT");
        }

        // File values used when env unset.
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.http_port, 8080);
        assert!(cfg.control_tls);
        assert_eq!(cfg.tcp_port_range, PortRange::new(21000, 21002));
        assert_eq!(cfg.udp_port_range, PortRange::new(31000, 31002));

        // Env wins when set.
        let _http = EnvGuard::set("DENIA_HTTP_PORT", "9090");
        let _tls = EnvGuard::set("DENIA_CONTROL_TLS", "false");
        let _tcp = EnvGuard::set("DENIA_TCP_PORT_RANGE", "22000-22001");
        let _udp = EnvGuard::set("DENIA_UDP_PORT_RANGE", "32000-32001");
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.http_port, 9090);
        assert!(!cfg.control_tls);
        assert_eq!(cfg.tcp_port_range, PortRange::new(22000, 22001));
        assert_eq!(cfg.udp_port_range, PortRange::new(32000, 32001));
    }

    #[test]
    fn invalid_l4_port_range_is_rejected() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        let _tcp = EnvGuard::set("DENIA_TCP_PORT_RANGE", "30000-20000");

        assert!(matches!(
            AppConfig::from_env(),
            Err(ConfigError::InvalidPortRange {
                name: "DENIA_TCP_PORT_RANGE",
                ..
            })
        ));
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

    #[test]
    fn upload_staging_defaults() {
        let c = AppConfig::for_test("0123456789012345678901234567890123");
        assert_eq!(c.uploads_dir, c.data_dir.join("uploads"));
        assert_eq!(c.upload_max_bytes, 536_870_912);
        assert_eq!(c.upload_max_entries, 200_000);
    }

    // The env-only / `cargo run` default DB path must match the layout
    // `denia setup` provisions (`<data_dir>/sqlite/denia.sqlite3`), not the old
    // `<data_dir>/denia.sqlite3` that disagreed with config.toml.in.
    #[test]
    fn database_path_default_matches_provisioned_sqlite_subdir() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        let _data_dir = EnvGuard::set("DENIA_DATA_DIR", "/var/lib/denia");
        unsafe {
            std::env::remove_var("DENIA_DATABASE_PATH");
        }
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(
            cfg.database_path,
            PathBuf::from("/var/lib/denia/sqlite/denia.sqlite3"),
        );
    }

    // Explicit DENIA_DATABASE_PATH still wins over the computed default.
    #[test]
    fn database_path_env_override_wins() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        let _db = EnvGuard::set("DENIA_DATABASE_PATH", "/custom/denia.db");
        let cfg = AppConfig::from_env().expect("config from env");
        assert_eq!(cfg.database_path, PathBuf::from("/custom/denia.db"));
    }

    #[test]
    fn invalid_control_domain_is_rejected() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        let _cd = EnvGuard::set("DENIA_CONTROL_DOMAIN", "has space.example.com");
        assert!(matches!(
            AppConfig::from_env(),
            Err(ConfigError::InvalidControlDomain(_))
        ));
    }

    #[test]
    fn valid_control_domain_is_lowercased() {
        let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_cfg_dir, _cfg_file) = isolated_config_file();
        let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
        let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
        let _cd = EnvGuard::set("DENIA_CONTROL_DOMAIN", "Denia.Example.COM");
        let cfg = AppConfig::from_env().expect("valid control domain");
        assert_eq!(cfg.control_domain.as_deref(), Some("denia.example.com"));
    }
}
