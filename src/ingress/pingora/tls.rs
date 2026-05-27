//! Per-SNI dynamic certificate selection for the `:443` listener.
//!
//! [`DeniaCertResolver`] implements Pingora's [`TlsAccept`] callback: at
//! handshake time it reads the SNI, looks up the matching cert in the
//! `ArcSwap<CertStore>` snapshot, and installs it via `ssl_use_certificate` /
//! `ssl_use_private_key`. When no cert exists for the SNI (or no SNI was sent),
//! it installs nothing and the handshake fails cleanly with
//! `TLSHandshakeFailure` — no default/wrong cert is ever leaked (Spike 0.3).
//!
//! ## Testability
//!
//! The selection decision (`resolve_sni_cert`) is a pure function over a
//! `CertStore` snapshot and an optional SNI, unit-tested directly without a live
//! TLS handshake. The `certificate_callback` is a thin wrapper that pulls the
//! SNI off the `SslRef`, calls `resolve_sni_cert`, and installs the result.

use std::sync::Arc;

use async_trait::async_trait;
use pingora::listeners::TlsAccept;
use pingora::protocols::tls::TlsRef;
use pingora::tls::ext::{ssl_use_certificate, ssl_use_private_key};
use pingora::tls::ssl::NameType;

use super::state::{CertStore, IngressState, ParsedCert};

/// Resolve the certificate to serve for a handshake SNI.
///
/// Returns `None` (→ decline the handshake) when no SNI was presented or the
/// SNI has no cert in the store. The lookup is case-insensitive (the store
/// lowercases its keys; `CertStore::get` lowercases the argument — audit A2).
pub fn resolve_sni_cert(store: &CertStore, sni: Option<&str>) -> Option<ParsedCert> {
    let sni = sni?;
    store.get(sni).cloned()
}

/// `TlsAccept` callback backed by the shared `IngressState` cert store.
pub struct DeniaCertResolver {
    state: Arc<IngressState>,
}

impl DeniaCertResolver {
    pub fn new(state: Arc<IngressState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl TlsAccept for DeniaCertResolver {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        let sni = ssl.servername(NameType::HOST_NAME).map(str::to_string);
        let store = self.state.certs();
        let Some(cert) = resolve_sni_cert(&store, sni.as_deref()) else {
            // Decline: install no cert → clean TLSHandshakeFailure. Never leak a
            // default cert for an unknown SNI.
            return;
        };
        install_cert(ssl, &cert);
    }
}

/// Install a parsed cert chain + key into the handshake `SslRef`.
///
/// Installs the leaf first, then each intermediate. Errors are swallowed
/// (logging key material is forbidden); a failed install simply leaves the
/// handshake without a cert, which fails closed.
fn install_cert(ssl: &mut TlsRef, cert: &ParsedCert) {
    if let Some(leaf) = cert.chain.first()
        && ssl_use_certificate(ssl, leaf).is_err()
    {
        return;
    }
    let _ = ssl_use_private_key(ssl, &cert.key);
    // Intermediates (chain[1..]) extend the chain; the leaf was set above via
    // ssl_use_certificate.
    for extra in cert.chain.iter().skip(1) {
        let _ = ssl.add_chain_cert(extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingora::tls::pkey::PKey;
    use pingora::tls::x509::X509;

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

    fn fake_cert() -> ParsedCert {
        ParsedCert {
            chain: vec![X509::from_pem(TEST_CERT_PEM).unwrap()],
            key: PKey::private_key_from_pem(TEST_KEY_PEM).unwrap(),
        }
    }

    fn store_with(sni: &str) -> CertStore {
        let mut s = CertStore::default();
        s.try_insert(sni, fake_cert()).unwrap();
        s
    }

    #[test]
    fn selects_cert_for_known_sni() {
        let store = store_with("api.example.com");
        assert!(resolve_sni_cert(&store, Some("api.example.com")).is_some());
    }

    #[test]
    fn selection_is_case_insensitive() {
        let store = store_with("api.example.com");
        assert!(resolve_sni_cert(&store, Some("API.EXAMPLE.COM")).is_some());
    }

    #[test]
    fn declines_unknown_sni() {
        let store = store_with("api.example.com");
        assert!(resolve_sni_cert(&store, Some("other.example.com")).is_none());
    }

    #[test]
    fn declines_when_no_sni_presented() {
        let store = store_with("api.example.com");
        assert!(resolve_sni_cert(&store, None).is_none());
    }

    #[test]
    fn declines_against_empty_store() {
        let store = CertStore::default();
        assert!(resolve_sni_cert(&store, Some("api.example.com")).is_none());
    }
}
