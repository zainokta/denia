//! In-process ACME (instant-acme, HTTP-01) issuance, persistence and renewal.
//!
//! Strict separation (per the design spec): cert *issuance* is async and fully
//! out-of-band here; cert *selection* is the sync `TlsAccept` callback in
//! `tls.rs`. This module:
//!
//! - loads/creates the ACME account key at `<tls_dir>/account.key` (mode 0600),
//! - drives an HTTP-01 order for a domain, publishing `token -> key
//!   authorization` into a [`ChallengeStore`] shared with the axum
//!   acme-challenge handler,
//! - persists issued certs atomically (temp + rename) at
//!   `<tls_dir>/<domain>/{fullchain.pem,key.pem}` mode 0600,
//! - loads certs from disk into a [`CertStore`] at boot,
//! - selects certs within the renewal window for re-order.
//!
//! ## Secrets discipline (audited)
//!
//! Key authorizations, the ACME account key, and cert private keys are never
//! logged and never `Debug`/`Serialize`d. [`IssuedCert`] and [`ChallengeStore`]
//! deliberately omit those derives. Account and leaf key files are written
//! atomically at mode 0600 so they are never world-readable mid-write.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use instant_acme::{Account, ChallengeType, Identifier, Key, NewOrder, OrderStatus, RetryPolicy};
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use thiserror::Error;

use pingora::tls::x509::X509;

use super::state::{CertStore, IngressError, ParsedCert, validate_domain};

/// Renewal window: certificates whose `notAfter` is within this many days are
/// selected for re-order.
pub const RENEWAL_WINDOW_DAYS: u32 = 30;

/// Filename for the persisted ACME account key (PKCS#8 DER), under `tls_dir`.
const ACCOUNT_KEY_FILE: &str = "account.key";
const FULLCHAIN_FILE: &str = "fullchain.pem";
const KEY_FILE: &str = "key.pem";

/// Typed errors at the ACME boundary.
#[derive(Debug, Error)]
pub enum AcmeError {
    #[error("invalid domain: {0}")]
    InvalidDomain(String),
    #[error("acme account email is required to issue certificates")]
    EmailRequired,
    #[error("acme protocol error: {0}")]
    Acme(String),
    #[error("acme order did not become valid: {0:?}")]
    OrderNotValid(OrderStatus),
    #[error("no http-01 challenge offered for {0}")]
    NoHttp01Challenge(String),
    #[error("certificate persistence failed: {0}")]
    Io(String),
    #[error("failed to parse issued certificate chain")]
    ParseChain,
}

impl From<IngressError> for AcmeError {
    fn from(e: IngressError) -> Self {
        match e {
            IngressError::InvalidDomain(d) => AcmeError::InvalidDomain(d),
            other => AcmeError::InvalidDomain(other.to_string()),
        }
    }
}

impl From<instant_acme::Error> for AcmeError {
    fn from(e: instant_acme::Error) -> Self {
        AcmeError::Acme(e.to_string())
    }
}

/// Shared `token -> key authorization` map for HTTP-01 challenges.
///
/// Owned by the ACME driver and cloned into the axum acme-challenge handler
/// (both hold the same `Arc`). Key authorizations are secrets: this type holds
/// them behind a lock and never derives `Debug`/`Serialize`.
#[derive(Clone, Default)]
pub struct ChallengeStore {
    inner: Arc<RwLock<BTreeMap<String, String>>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a `token -> key_authorization` mapping for an in-flight order.
    pub fn register(&self, token: impl Into<String>, key_authorization: impl Into<String>) {
        if let Ok(mut map) = self.inner.write() {
            map.insert(token.into(), key_authorization.into());
        }
    }

    /// Look up the key authorization for a token (used by the axum handler).
    pub fn get(&self, token: &str) -> Option<String> {
        self.inner.read().ok().and_then(|m| m.get(token).cloned())
    }

    /// Drop a token after its order completes (or fails), so the store does not
    /// retain key authorizations indefinitely.
    pub fn remove(&self, token: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.remove(token);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }
}

/// A freshly issued certificate: PEM chain (leaf first) + PEM private key.
///
/// Holds private key material in `key_pem`, so it intentionally has no
/// `Debug`/`Serialize` derive (secrets discipline).
pub struct IssuedCert {
    pub fullchain_pem: String,
    pub key_pem: String,
}

/// Driver for ACME issuance against a configured directory.
pub struct AcmeDriver {
    account: Account,
    challenges: ChallengeStore,
}

impl AcmeDriver {
    /// Construct a driver, loading or creating the ACME account key at
    /// `<tls_dir>/account.key` (mode 0600) and registering/binding the account
    /// on the directory.
    ///
    /// `email` is required (HTTP-01 issuance needs a contact); callers gate this
    /// via [`crate::config::AppConfig::require_acme_email`] at startup, but we
    /// also reject an empty email here.
    pub async fn new(
        tls_dir: &Path,
        directory_url: &str,
        email: &str,
        challenges: ChallengeStore,
    ) -> Result<Self, AcmeError> {
        if email.trim().is_empty() {
            return Err(AcmeError::EmailRequired);
        }
        std::fs::create_dir_all(tls_dir).map_err(|e| AcmeError::Io(e.to_string()))?;
        let account_key_path = tls_dir.join(ACCOUNT_KEY_FILE);

        // Load or create the account key, then build the key pair twice from the
        // same DER: `Key` (for signing) and `PrivateKeyDer` (for the builder).
        let der = load_or_create_account_key_der(&account_key_path)?;
        let key = Key::from_pkcs8_der(der.clone_key()).map_err(AcmeError::from)?;
        let contact = format!("mailto:{email}");
        let (account, _credentials) = Account::builder()
            .map_err(AcmeError::from)?
            .create_from_key((key, PrivateKeyDer::Pkcs8(der)), directory_url.to_string())
            .await
            .map_err(AcmeError::from)?;
        // Best-effort contact registration; ignore failure to avoid blocking
        // issuance if the directory rejects contact updates.
        let _ = account.update_contacts(&[contact.as_str()]).await;

        Ok(Self {
            account,
            challenges,
        })
    }

    /// Clone the shared challenge store (for wiring into the axum handler).
    pub fn challenges(&self) -> ChallengeStore {
        self.challenges.clone()
    }

    /// Drive an HTTP-01 order for `domain` to completion and return the issued
    /// chain + key. Validates the domain before it becomes an order identifier
    /// (audit A1). Network-bound; gated out of the default test run.
    pub async fn issue(&self, domain: &str) -> Result<IssuedCert, AcmeError> {
        let domain = validate_domain(domain)?;
        let identifiers = [Identifier::Dns(domain.clone())];
        let mut order = self
            .account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(AcmeError::from)?;

        // Publish the HTTP-01 key authorization for each authorization, then
        // mark each challenge ready.
        let mut tokens: Vec<String> = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(authz) = authorizations.next().await {
            let mut authz = authz.map_err(AcmeError::from)?;
            let mut challenge = authz
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| AcmeError::NoHttp01Challenge(domain.clone()))?;
            let token = challenge.token.clone();
            let key_auth = challenge.key_authorization();
            self.challenges.register(token.clone(), key_auth.as_str());
            tokens.push(token);
            challenge.set_ready().await.map_err(AcmeError::from)?;
        }

        let result = self.finalize_order(&mut order, &domain).await;
        // Always clear published tokens once the order resolves.
        for token in &tokens {
            self.challenges.remove(token);
        }
        result
    }

    async fn finalize_order(
        &self,
        order: &mut instant_acme::Order,
        domain: &str,
    ) -> Result<IssuedCert, AcmeError> {
        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(AcmeError::from)?;
        if status != OrderStatus::Ready {
            return Err(AcmeError::OrderNotValid(status));
        }
        // `finalize()` (rcgen feature) generates the leaf keypair + CSR and
        // returns the private key PEM; we keep it to persist alongside the chain.
        let key_pem = order.finalize().await.map_err(AcmeError::from)?;
        let fullchain_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(AcmeError::from)?;
        let _ = domain;
        Ok(IssuedCert {
            fullchain_pem,
            key_pem,
        })
    }
}

/// Load the PKCS#8 DER account key from `path`, generating and persisting a new
/// one (mode 0600, atomic) if absent.
fn load_or_create_account_key_der(path: &Path) -> Result<PrivatePkcs8KeyDer<'static>, AcmeError> {
    if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| AcmeError::Io(e.to_string()))?;
        return Ok(PrivatePkcs8KeyDer::from(bytes));
    }
    let (_key, pkcs8) = Key::generate_pkcs8().map_err(AcmeError::from)?;
    write_secret_file(path, pkcs8.secret_pkcs8_der())?;
    Ok(pkcs8.clone_key())
}

/// Persist an issued certificate atomically under `<tls_dir>/<domain>/`.
///
/// Writes `fullchain.pem` and `key.pem` at mode 0600 via temp-file + rename so
/// a reader never observes a partial or world-readable key. `domain` is
/// validated/lowercased so it cannot escape `tls_dir` (no `..`, no separators —
/// [`validate_domain`] rejects them).
pub fn persist_cert(tls_dir: &Path, domain: &str, cert: &IssuedCert) -> Result<PathBuf, AcmeError> {
    let domain = validate_domain(domain)?;
    let dir = tls_dir.join(&domain);
    std::fs::create_dir_all(&dir).map_err(|e| AcmeError::Io(e.to_string()))?;
    write_secret_file(&dir.join(FULLCHAIN_FILE), cert.fullchain_pem.as_bytes())?;
    write_secret_file(&dir.join(KEY_FILE), cert.key_pem.as_bytes())?;
    Ok(dir)
}

/// Atomically write `bytes` to `path` at mode 0600 (temp file in the same
/// directory + `rename`). The temp file is created with the restrictive mode up
/// front so the secret is never momentarily world-readable.
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), AcmeError> {
    let dir = path
        .parent()
        .ok_or_else(|| AcmeError::Io("path has no parent directory".to_string()))?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("denia"),
        std::process::id()
    ));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| AcmeError::Io(e.to_string()))?;
        f.write_all(bytes)
            .map_err(|e| AcmeError::Io(e.to_string()))?;
        f.sync_all().map_err(|e| AcmeError::Io(e.to_string()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        AcmeError::Io(e.to_string())
    })?;
    Ok(())
}

/// Load all persisted certs from `<tls_dir>/<domain>/` into a [`CertStore`].
///
/// Each subdirectory whose name validates as a domain and that holds both
/// `fullchain.pem` and `key.pem` is parsed and inserted (SNI = directory name).
/// Unparseable or malformed entries are skipped (not fatal): a single corrupt
/// cert must not stop the proxy from serving the rest.
pub fn load_certs_from_disk(tls_dir: &Path) -> CertStore {
    let mut store = CertStore::default();
    let Ok(entries) = std::fs::read_dir(tls_dir) else {
        return store;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if validate_domain(name).is_err() {
            continue;
        }
        match load_cert_dir(&path) {
            Some(parsed) => {
                let _ = store.try_insert(name, parsed);
            }
            None => continue,
        }
    }
    store
}

/// Parse `fullchain.pem` + `key.pem` from a single cert directory into a
/// [`ParsedCert`]. Returns `None` if either file is missing or unparseable.
fn load_cert_dir(dir: &Path) -> Option<ParsedCert> {
    let chain_pem = std::fs::read(dir.join(FULLCHAIN_FILE)).ok()?;
    let key_pem = std::fs::read(dir.join(KEY_FILE)).ok()?;
    parse_parsed_cert(&chain_pem, &key_pem)
}

/// Parse a PEM chain + PEM private key into a [`ParsedCert`] (leaf first).
pub fn parse_parsed_cert(chain_pem: &[u8], key_pem: &[u8]) -> Option<ParsedCert> {
    let chain = X509::stack_from_pem(chain_pem).ok()?;
    if chain.is_empty() {
        return None;
    }
    let key = pingora::tls::pkey::PKey::private_key_from_pem(key_pem).ok()?;
    Some(ParsedCert { chain, key })
}

/// Whether a certificate whose `notAfter` is `not_after` should be renewed now,
/// i.e. it expires within `window_days`.
///
/// Pure decision step (extracted for unit testing): returns `true` when
/// `not_after <= now + window_days`.
pub fn needs_renewal(
    not_after: &boring::asn1::Asn1TimeRef,
    window_days: u32,
) -> Result<bool, AcmeError> {
    let threshold = boring::asn1::Asn1Time::days_from_now(window_days)
        .map_err(|e| AcmeError::Acme(e.to_string()))?;
    // Renew if the cert expires at or before the threshold.
    Ok(not_after < threshold.as_ref())
}

/// Select the SNI names in `store` whose leaf cert is within the renewal window
/// and should be re-ordered. Skips entries whose `notAfter` cannot be evaluated.
pub fn select_renewals(store: &CertStore, window_days: u32) -> Vec<String> {
    let mut out = Vec::new();
    for sni in store.sni_names() {
        if let Some(cert) = store.get(&sni)
            && let Some(leaf) = cert.chain.first()
            && matches!(needs_renewal(leaf.not_after(), window_days), Ok(true))
        {
            out.push(sni);
        }
    }
    out
}

#[cfg(test)]
mod challenge_store_tests {
    use super::*;

    #[test]
    fn register_and_get_roundtrip() {
        let store = ChallengeStore::new();
        store.register("tok-1", "tok-1.keyauth");
        assert_eq!(store.get("tok-1").as_deref(), Some("tok-1.keyauth"));
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn remove_drops_token() {
        let store = ChallengeStore::new();
        store.register("tok", "auth");
        assert_eq!(store.len(), 1);
        store.remove("tok");
        assert_eq!(store.len(), 0);
        assert!(store.get("tok").is_none());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Throwaway self-signed material (matches the state.rs test PEMs; valid
    /// until 2036). NOT a secret — only used to exercise parsing/persistence.
    const TEST_KEY_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgZFwD6luyekuuSrp6\n\
jir4r0J1o+Lb2L1YFBR7HBJHCE2hRANCAATBJ6iTtPrDFPLnqcNA/87722/N255n\n\
xDZ2oRsDFpP735ud77NSPM8v0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rn\n\
-----END PRIVATE KEY-----\n";
    const TEST_CERT_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBfzCCASWgAwIBAgIUT2TFIC8WbUryUcwKjixECF5vQoswCgYIKoZIzj0EAwIw\n\
FTETMBEGA1UEAwwKZGVuaWEtdGVzdDAeFw0yNjA1MjcxODQ1MjNaFw0zNjA1MjQx\n\
ODQ1MjNaMBUxEzARBgNVBAMMCmRlbmlhLXRlc3QwWTATBgcqhkjOPQIBBggqhkjO\n\
PQMBBwNCAATBJ6iTtPrDFPLnqcNA/87722/N255nxDZ2oRsDFpP735ud77NSPM8v\n\
0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rno1MwUTAdBgNVHQ4EFgQUQ+pPRiWYnXOs\n\
F7Gt+6mn7TM+MOYwHwYDVR0jBBgwFoAUQ+pPRiWYnXOsF7Gt+6mn7TM+MOYwDwYD\n\
VR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiEA7rINC49fiLX2DYAE06Cm\n\
7WYc7cctlyaUC0Nr9HUIgkQCIDQkV/AqQqzeDIL0B1zFwp8gttKI+dcUY0EOFPnf\n\
/bBZ\n\
-----END CERTIFICATE-----\n";

    fn issued() -> IssuedCert {
        IssuedCert {
            fullchain_pem: String::from_utf8(TEST_CERT_PEM.to_vec()).unwrap(),
            key_pem: String::from_utf8(TEST_KEY_PEM.to_vec()).unwrap(),
        }
    }

    #[test]
    fn persist_cert_writes_files_at_mode_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = persist_cert(tmp.path(), "api.example.com", &issued()).unwrap();
        assert_eq!(dir, tmp.path().join("api.example.com"));

        for name in [FULLCHAIN_FILE, KEY_FILE] {
            let p = dir.join(name);
            assert!(p.exists(), "{name} written");
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{name} must be 0600, got {mode:o}");
        }
    }

    #[test]
    fn persist_cert_is_atomic_no_temp_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        persist_cert(tmp.path(), "api.example.com", &issued()).unwrap();
        let dir = tmp.path().join("api.example.com");
        // No stray temp files (`.fullchain.pem.tmp.*`) survive the rename.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp files must be renamed away");
    }

    #[test]
    fn persist_cert_rejects_path_traversal_domain() {
        let tmp = tempfile::tempdir().unwrap();
        let err = persist_cert(tmp.path(), "../escape", &issued());
        assert!(matches!(err, Err(AcmeError::InvalidDomain(_))));
    }

    #[test]
    fn load_certs_from_disk_round_trips_persisted_cert() {
        let tmp = tempfile::tempdir().unwrap();
        persist_cert(tmp.path(), "api.example.com", &issued()).unwrap();
        persist_cert(tmp.path(), "www.example.com", &issued()).unwrap();

        let store = load_certs_from_disk(tmp.path());
        assert_eq!(store.len(), 2);
        assert!(store.get("api.example.com").is_some());
        assert!(store.get("www.example.com").is_some());
    }

    #[test]
    fn load_certs_from_disk_skips_non_domain_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        // The account key file is not a directory and must be ignored.
        write_secret_file(&tmp.path().join(ACCOUNT_KEY_FILE), b"junk").unwrap();
        std::fs::create_dir_all(tmp.path().join("not a domain")).unwrap();
        persist_cert(tmp.path(), "ok.example.com", &issued()).unwrap();

        let store = load_certs_from_disk(tmp.path());
        assert_eq!(store.len(), 1);
        assert!(store.get("ok.example.com").is_some());
    }

    #[test]
    fn account_key_persisted_at_mode_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(ACCOUNT_KEY_FILE);
        let der = load_or_create_account_key_der(&path).unwrap();
        // The freshly generated DER round-trips into a usable signing key.
        Key::from_pkcs8_der(der.clone_key()).expect("valid generated account key");
        assert!(path.exists());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Reload reads the same key file (idempotent, no overwrite).
        let again = load_or_create_account_key_der(&path).unwrap();
        Key::from_pkcs8_der(again.clone_key()).expect("reloaded account key");
    }
}

#[cfg(test)]
mod renewal_tests {
    use super::*;

    const TEST_KEY_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgZFwD6luyekuuSrp6\n\
jir4r0J1o+Lb2L1YFBR7HBJHCE2hRANCAATBJ6iTtPrDFPLnqcNA/87722/N255n\n\
xDZ2oRsDFpP735ud77NSPM8v0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rn\n\
-----END PRIVATE KEY-----\n";
    const TEST_CERT_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBfzCCASWgAwIBAgIUT2TFIC8WbUryUcwKjixECF5vQoswCgYIKoZIzj0EAwIw\n\
FTETMBEGA1UEAwwKZGVuaWEtdGVzdDAeFw0yNjA1MjcxODQ1MjNaFw0zNjA1MjQx\n\
ODQ1MjNaMBUxEzARBgNVBAMMCmRlbmlhLXRlc3QwWTATBgcqhkjOPQIBBggqhkjO\n\
PQMBBwNCAATBJ6iTtPrDFPLnqcNA/87722/N255nxDZ2oRsDFpP735ud77NSPM8v\n\
0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rno1MwUTAdBgNVHQ4EFgQUQ+pPRiWYnXOs\n\
F7Gt+6mn7TM+MOYwHwYDVR0jBBgwFoAUQ+pPRiWYnXOsF7Gt+6mn7TM+MOYwDwYD\n\
VR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiEA7rINC49fiLX2DYAE06Cm\n\
7WYc7cctlyaUC0Nr9HUIgkQCIDQkV/AqQqzeDIL0B1zFwp8gttKI+dcUY0EOFPnf\n\
/bBZ\n\
-----END CERTIFICATE-----\n";

    fn store_with_test_cert() -> CertStore {
        let mut store = CertStore::default();
        let parsed = parse_parsed_cert(TEST_CERT_PEM, TEST_KEY_PEM).expect("parse");
        store.try_insert("denia-test.example.com", parsed).unwrap();
        store
    }

    #[test]
    fn cert_far_from_expiry_not_renewed() {
        // The test cert expires in 2036; a 30-day window must not select it.
        let store = store_with_test_cert();
        assert!(select_renewals(&store, RENEWAL_WINDOW_DAYS).is_empty());
    }

    #[test]
    fn cert_within_window_is_selected() {
        // A window wider than the cert's remaining lifetime selects it.
        let store = store_with_test_cert();
        let selected = select_renewals(&store, 365 * 100);
        assert_eq!(selected, vec!["denia-test.example.com".to_string()]);
    }

    #[test]
    fn needs_renewal_true_for_huge_window() {
        let parsed = parse_parsed_cert(TEST_CERT_PEM, TEST_KEY_PEM).unwrap();
        let leaf = &parsed.chain[0];
        assert!(needs_renewal(leaf.not_after(), 365 * 100).unwrap());
        assert!(!needs_renewal(leaf.not_after(), 1).unwrap());
    }
}

/// Network-bound ACME integration test against a pebble / LE-staging directory.
///
/// Gated behind `DENIA_RUN_ACME_NET_TESTS=1` (and `#[ignore]`) like the
/// privileged runtime tests, so the default `cargo test` run never touches the
/// network. Requires `DENIA_ACME_DIRECTORY_URL`, `DENIA_ACME_TEST_DOMAIN`, and
/// `DENIA_ACME_EMAIL` to be set, plus a reachable HTTP-01 responder serving the
/// published challenge map. See the privileged end-to-end test (Phase 8) for the
/// full wired path; this one exercises `AcmeDriver::issue` directly.
#[cfg(test)]
mod net_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "network: set DENIA_RUN_ACME_NET_TESTS=1 and ACME env to run"]
    async fn issue_against_directory() {
        if std::env::var("DENIA_RUN_ACME_NET_TESTS").as_deref() != Ok("1") {
            eprintln!("skipping: DENIA_RUN_ACME_NET_TESTS != 1");
            return;
        }
        let directory_url = std::env::var("DENIA_ACME_DIRECTORY_URL")
            .expect("DENIA_ACME_DIRECTORY_URL must be set for the net test");
        let domain = std::env::var("DENIA_ACME_TEST_DOMAIN")
            .expect("DENIA_ACME_TEST_DOMAIN must be set for the net test");
        let email = std::env::var("DENIA_ACME_EMAIL")
            .expect("DENIA_ACME_EMAIL must be set for the net test");

        let tmp = tempfile::tempdir().unwrap();
        let challenges = ChallengeStore::new();
        let driver = AcmeDriver::new(tmp.path(), &directory_url, &email, challenges)
            .await
            .expect("build acme driver");
        let issued = driver.issue(&domain).await.expect("issue cert");
        assert!(issued.fullchain_pem.contains("BEGIN CERTIFICATE"));
        // Persisted material is parseable and selectable by SNI.
        persist_cert(tmp.path(), &domain, &issued).unwrap();
        let store = load_certs_from_disk(tmp.path());
        assert!(store.get(&domain).is_some());
    }
}
