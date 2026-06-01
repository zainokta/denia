use std::time::Duration;
use subtle::ConstantTimeEq;

use crate::verification::error::DomainVerifyError;
use crate::verification::verifier::DomainVerifier;

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
        if hostname.parse::<std::net::IpAddr>().is_ok() {
            return Err(DomainVerifyError::DnsLookupFailed);
        }

        let url = match &self.base_url_override {
            Some(base) => format!("{base}/.well-known/denia-challenge/{token}"),
            None => {
                let resolved: Vec<std::net::SocketAddr> =
                    tokio::net::lookup_host((hostname, 80u16))
                        .await
                        .map_err(|_| DomainVerifyError::DnsLookupFailed)?
                        .collect();
                if resolved.is_empty() {
                    return Err(DomainVerifyError::DnsLookupFailed);
                }
                for addr in &resolved {
                    if is_internal_ip(&addr.ip()) {
                        return Err(DomainVerifyError::DnsLookupFailed);
                    }
                }
                let target = resolved[0];
                format!("http://{target}/.well-known/denia-challenge/{token}")
            }
        };

        let mut request = self.client.get(&url);
        if self.base_url_override.is_none() {
            request = request.header("Host", hostname);
        }

        let response = request.send().await.map_err(|e| {
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

        let bytes = read_limited_body(response).await?;
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

async fn read_limited_body(mut response: reqwest::Response) -> Result<Vec<u8>, DomainVerifyError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DomainVerifyError::BodyTooLarge)?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(DomainVerifyError::BodyTooLarge)?;
        if next_len > MAX_BODY {
            return Err(DomainVerifyError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn is_internal_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_ip(&std::net::IpAddr::V4(v4));
            }
            let octets = v6.octets();
            v6.is_loopback()
                || v6.is_unspecified()
                || (octets[0] & 0xfe) == 0xfc
                || octets[0] == 0xff
                || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
        }
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
                    .path_includes("/.well-known/denia-challenge/");
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

    #[tokio::test]
    async fn verifier_body_reader_stops_after_limit() {
        let server = MockServer::start_async().await;
        let big = "x".repeat(MAX_BODY + 1);
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/.well-known/denia-challenge/tok");
                then.status(200).body(big);
            })
            .await;
        let v = client_with_base(&server.base_url());
        let response = v
            .client
            .get(format!(
                "{}/.well-known/denia-challenge/tok",
                server.base_url()
            ))
            .send()
            .await
            .expect("response");

        let err = read_limited_body(response).await.unwrap_err();

        assert_eq!(err, DomainVerifyError::BodyTooLarge);
    }

    #[test]
    fn internal_ip_filter_rejects_ipv6_private_and_multicast_ranges() {
        for addr in ["fc00::1", "fd00::1", "ff02::1", "::ffff:127.0.0.1"] {
            let ip = addr.parse().unwrap();
            assert!(is_internal_ip(&ip), "{addr} must be blocked");
        }
    }
}
