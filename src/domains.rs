use rand::Rng;
use std::time::Duration;
use subtle::ConstantTimeEq;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostnameError {
    #[error("hostname is empty")]
    Empty,
    #[error("hostname contains invalid character")]
    InvalidCharacter,
    #[error("hostname is not a fully qualified domain name")]
    NotFqdn,
    #[error("hostname looks like an IP address or includes a port or path")]
    NotHostname,
}

pub fn validate_hostname(input: &str) -> Result<String, HostnameError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(HostnameError::Empty);
    }
    if trimmed.contains('`') || trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(HostnameError::InvalidCharacter);
    }
    if trimmed.contains('/') || trimmed.contains(':') || trimmed.contains(' ') {
        return Err(HostnameError::NotHostname);
    }
    if trimmed.parse::<std::net::IpAddr>().is_ok() {
        return Err(HostnameError::NotHostname);
    }
    let labels: Vec<&str> = trimmed.split('.').collect();
    if labels.len() < 2 {
        return Err(HostnameError::NotFqdn);
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err(HostnameError::InvalidCharacter);
        }
        let bytes = label.as_bytes();
        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err(HostnameError::InvalidCharacter);
        }
        for b in bytes {
            let ok = b.is_ascii_alphanumeric() || *b == b'-';
            if !ok {
                return Err(HostnameError::InvalidCharacter);
            }
            if b.is_ascii_uppercase() {
                return Err(HostnameError::InvalidCharacter);
            }
        }
    }
    Ok(trimmed.to_string())
}

pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainVerifyError {
    #[error("dns lookup failed")]
    DnsLookupFailed,
    #[error("connection refused")]
    ConnectionRefused,
    #[error("connection timeout")]
    ConnectionTimeout,
    #[error("http {0}")]
    HttpStatus(u16),
    #[error("body mismatch")]
    BodyMismatch,
    #[error("body too large")]
    BodyTooLarge,
}

#[async_trait::async_trait]
pub trait DomainVerifier: Send + Sync {
    async fn verify(&self, hostname: &str, token: &str) -> Result<(), DomainVerifyError>;
}

const MAX_BODY: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HttpDomainVerifier {
    client: reqwest::Client,
    base_url_override: Option<String>,
}

impl HttpDomainVerifier {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .user_agent("denia-verifier/1")
            .build()
            .expect("reqwest client");
        Self {
            client,
            base_url_override: None,
        }
    }

    #[cfg(test)]
    pub fn with_base_url(base: String) -> Self {
        let mut v = Self::new();
        v.base_url_override = Some(base);
        v
    }
}

impl Default for HttpDomainVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl DomainVerifier for HttpDomainVerifier {
    async fn verify(&self, hostname: &str, token: &str) -> Result<(), DomainVerifyError> {
        let base = match &self.base_url_override {
            Some(b) => b.clone(),
            None => format!("http://{hostname}"),
        };
        let url = format!("{base}/.well-known/denia-challenge/{token}");

        let response = self.client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                DomainVerifyError::ConnectionTimeout
            } else if e.is_connect() {
                let msg = e.to_string().to_lowercase();
                if msg.contains("dns") || msg.contains("name") {
                    DomainVerifyError::DnsLookupFailed
                } else {
                    DomainVerifyError::ConnectionRefused
                }
            } else {
                DomainVerifyError::ConnectionRefused
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(DomainVerifyError::HttpStatus(status.as_u16()));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|_| DomainVerifyError::BodyTooLarge)?;
        if bytes.len() > MAX_BODY {
            return Err(DomainVerifyError::BodyTooLarge);
        }
        let trimmed = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        let expected = token.as_bytes();
        if trimmed.len() != expected.len() {
            return Err(DomainVerifyError::BodyMismatch);
        }
        if trimmed.ct_eq(expected).unwrap_u8() != 1 {
            return Err(DomainVerifyError::BodyMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod verifier_tests {
    use super::*;
    use httpmock::prelude::*;

    fn client_with_base(base: &str) -> HttpDomainVerifier {
        HttpDomainVerifier::with_base_url(base.to_string())
    }

    #[tokio::test]
    async fn verifier_success() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/.well-known/denia-challenge/tok123");
                then.status(200).body("tok123");
            })
            .await;
        let v = client_with_base(&server.base_url());
        v.verify("ignored.example.com", "tok123").await.unwrap();
    }

    #[tokio::test]
    async fn verifier_404_returns_http_status() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path_contains("/.well-known/denia-challenge/");
                then.status(404);
            })
            .await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::HttpStatus(404));
    }

    #[tokio::test]
    async fn verifier_body_mismatch() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/.well-known/denia-challenge/tok123");
                then.status(200).body("wrong");
            })
            .await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok123").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::BodyMismatch);
    }

    #[tokio::test]
    async fn verifier_body_too_large() {
        let server = MockServer::start_async().await;
        let big = "x".repeat(2048);
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/.well-known/denia-challenge/tok");
                then.status(200).body(big);
            })
            .await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::BodyTooLarge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_hostname_accepts_fqdn() {
        assert_eq!(
            validate_hostname("app.example.com").unwrap(),
            "app.example.com"
        );
        assert_eq!(validate_hostname("a.b.co").unwrap(), "a.b.co");
    }

    #[test]
    fn validate_hostname_rejects_empty_and_single_label() {
        assert_eq!(validate_hostname("").unwrap_err(), HostnameError::Empty);
        assert_eq!(
            validate_hostname("localhost").unwrap_err(),
            HostnameError::NotFqdn
        );
    }

    #[test]
    fn validate_hostname_rejects_ip_port_path() {
        assert_eq!(
            validate_hostname("127.0.0.1").unwrap_err(),
            HostnameError::NotHostname
        );
        assert_eq!(
            validate_hostname("example.com:8080").unwrap_err(),
            HostnameError::NotHostname
        );
        assert_eq!(
            validate_hostname("example.com/path").unwrap_err(),
            HostnameError::NotHostname
        );
    }

    #[test]
    fn validate_hostname_rejects_uppercase_and_specials() {
        assert!(validate_hostname("APP.example.com").is_err());
        assert!(validate_hostname("app.example.com`").is_err());
        assert!(validate_hostname("app..example.com").is_err());
        assert!(validate_hostname("-leading.example.com").is_err());
        assert!(validate_hostname("trailing-.example.com").is_err());
    }

    #[test]
    fn generate_token_is_64_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(
            t.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let u = generate_token();
        assert_ne!(t, u, "tokens should be random");
    }
}
