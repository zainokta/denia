//! Pluggable DNS-01 ACME providers (ADR-038), modeled on Traefik/lego.
//!
//! DNS-01 proves domain ownership by writing a `_acme-challenge.<domain>` TXT
//! record, so it works for hostnames fronted by a proxy (Cloudflare orange-cloud,
//! a WAF) where the in-process HTTP-01 solver in `acme.rs` can never reach `:80`.
//!
//! The core stays provider-agnostic: a [`Dns01Provider`] does only `present` /
//! `cleanup` (lego's `Present`/`CleanUp`), while propagation confirmation and
//! CNAME delegation live in [`propagation`] and run the same for every provider.
//! [`build_provider`] is the factory selected by `DENIA_ACME_DNS_PROVIDER`.
//!
//! Providers shipped:
//! - [`cloudflare`] — Cloudflare API token (the default).
//! - [`exec`] — runs an operator script for present/cleanup; covers any DNS host
//!   (Akamai, Route53, PowerDNS, BIND, …) with no provider credential held by
//!   Denia. CNAME delegation lets a supported provider answer challenges for a
//!   domain whose apex DNS it does not host.
//!
//! ## Secrets discipline (audited)
//!
//! A provider's credential (e.g. the Cloudflare token) is held by that provider
//! only, never logged, and never `Debug`/`Serialize`d. [`Dns01Error`] carries
//! only zone/record names and HTTP status. The `exec` provider holds no
//! credential at all — the script owns its own auth.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AppConfig, DnsProviderKind};

pub mod cloudflare;
pub mod exec;
pub mod propagation;

pub use cloudflare::CloudflareDns01;
pub use exec::ExecDns01;
pub use propagation::PropagationCheck;

/// Typed errors at the DNS-01 provider boundary. Messages never include any
/// credential or the challenge value — only zone/record names and HTTP status.
#[derive(Debug, Error)]
pub enum Dns01Error {
    /// Transport-level failure (DNS, connect, timeout) — retryable.
    #[error("dns provider transport error: {0}")]
    Http(String),
    /// Authentication/authorization failure (401/403) — permanent; the credential
    /// is invalid or under-scoped. Not retryable until the operator fixes it.
    #[error("dns provider auth error (status {status}): {errors}")]
    Auth { status: u16, errors: String },
    /// Rate-limit / server-side failure (429, 5xx) — retryable.
    #[error("dns provider transient error (status {status}): {errors}")]
    Transient { status: u16, errors: String },
    /// Other API failure (4xx other than auth, or malformed body) — permanent.
    #[error("dns provider api error (status {status}): {errors}")]
    Api { status: u16, errors: String },
    #[error("no dns zone found for {0}")]
    ZoneNotFound(String),
    #[error("dns-01 TXT record did not propagate for {0} in time")]
    Propagation(String),
    /// CNAME chain for `_acme-challenge.<domain>` loops or exceeds the hop limit.
    #[error("dns-01 CNAME delegation error: {0}")]
    Cname(String),
    /// External `exec` provider script failed (non-zero exit / spawn error).
    #[error("dns-01 exec provider error: {0}")]
    Exec(String),
    /// The selected provider is missing required configuration.
    #[error("dns-01 provider not configured: {0}")]
    NotConfigured(String),
}

impl Dns01Error {
    /// Whether retrying the operation could succeed without operator action.
    /// Permanent errors (bad credential, unknown zone, bad config, script
    /// failure) are not worth retrying on a tight loop.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Dns01Error::Http(_) | Dns01Error::Transient { .. } | Dns01Error::Propagation(_)
        )
    }
}

/// A TXT record created by [`Dns01Provider::present`], carried to `cleanup`.
///
/// `provider_ref` holds provider-internal deletion info (Cloudflare:
/// `"zone_id:record_id"`; `exec`: `None`, cleanup re-derives from fqdn+value).
pub struct TxtHandle {
    pub fqdn: String,
    pub value: String,
    pub provider_ref: Option<String>,
}

/// A DNS-01 provider: create and remove the challenge TXT record. Mirrors lego's
/// `challenge.Provider` (`Present`/`CleanUp`) plus the optional `Timeout`.
#[async_trait]
pub trait Dns01Provider: Send + Sync {
    /// Create the TXT record `fqdn` = `value`; return a handle for cleanup.
    async fn present(&self, fqdn: &str, value: &str) -> Result<TxtHandle, Dns01Error>;
    /// Remove the record created by `present` (idempotent — a missing record is
    /// not an error).
    async fn cleanup(&self, handle: &TxtHandle) -> Result<(), Dns01Error>;
    /// Propagation `(timeout, polling interval)` for this provider. The initial
    /// delay before polling is configured separately (`acme_dns_propagation_secs`).
    fn timeout(&self) -> (Duration, Duration) {
        (Duration::from_secs(120), Duration::from_secs(2))
    }
}

/// The TXT record name for a DNS-01 challenge on `domain`.
pub fn txt_record_name(domain: &str) -> String {
    format!("_acme-challenge.{domain}")
}

/// Build the configured DNS-01 provider (factory, lego's `ByName`). Validated at
/// startup by [`crate::config::AppConfig::require_dns01_provider`]; the
/// `NotConfigured` arms are defensive.
pub fn build_provider(config: &AppConfig) -> Result<Arc<dyn Dns01Provider>, Dns01Error> {
    match config.acme_dns_provider {
        DnsProviderKind::Cloudflare => {
            let token = config
                .cf_dns_api_token
                .as_ref()
                .ok_or_else(|| Dns01Error::NotConfigured("cloudflare api token".to_string()))?;
            Ok(Arc::new(CloudflareDns01::new(token.expose().to_string())))
        }
        DnsProviderKind::Exec => {
            let cmd = config
                .acme_dns_exec
                .as_ref()
                .ok_or_else(|| Dns01Error::NotConfigured("exec script path".to_string()))?;
            Ok(Arc::new(ExecDns01::new(cmd.clone())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_record_name_prefixes_acme_challenge() {
        assert_eq!(
            txt_record_name("console.zainokta.com"),
            "_acme-challenge.console.zainokta.com"
        );
    }

    #[test]
    fn error_retryability() {
        assert!(Dns01Error::Http("x".into()).is_retryable());
        assert!(
            Dns01Error::Transient {
                status: 503,
                errors: "x".into()
            }
            .is_retryable()
        );
        assert!(Dns01Error::Propagation("d".into()).is_retryable());
        assert!(
            !Dns01Error::Auth {
                status: 403,
                errors: "x".into()
            }
            .is_retryable()
        );
        assert!(!Dns01Error::Exec("boom".into()).is_retryable());
        assert!(!Dns01Error::NotConfigured("x".into()).is_retryable());
    }

    #[test]
    fn build_provider_cloudflare_requires_token() {
        use crate::config::AppConfig;
        let mut c = AppConfig::for_test("0123456789012345678901234567890123");
        c.acme_dns_provider = DnsProviderKind::Cloudflare;
        c.cf_dns_api_token = None;
        assert!(matches!(
            build_provider(&c),
            Err(Dns01Error::NotConfigured(_))
        ));
        c.cf_dns_api_token = Some(crate::config::Secret::new("t"));
        assert!(build_provider(&c).is_ok());
    }

    #[test]
    fn build_provider_exec_requires_script() {
        use crate::config::AppConfig;
        let mut c = AppConfig::for_test("0123456789012345678901234567890123");
        c.acme_dns_provider = DnsProviderKind::Exec;
        c.acme_dns_exec = None;
        assert!(matches!(
            build_provider(&c),
            Err(Dns01Error::NotConfigured(_))
        ));
        c.acme_dns_exec = Some(std::path::PathBuf::from("/usr/local/bin/denia-dns-hook"));
        assert!(build_provider(&c).is_ok());
    }
}
