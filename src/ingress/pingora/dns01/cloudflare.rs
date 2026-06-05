//! Cloudflare DNS-01 provider: writes/removes the challenge TXT record via the
//! Cloudflare API v4. Implements [`Dns01Provider`]. Holds the secret API token
//! (no `Debug` derive); errors carry only zone/record names and HTTP status.

use async_trait::async_trait;
use serde::Deserialize;

use super::{Dns01Error, Dns01Provider, TxtHandle};

/// Default Cloudflare API v4 base URL. Overridable in tests via
/// [`CloudflareDns01::with_base_url`].
const DEFAULT_BASE_URL: &str = "https://api.cloudflare.com/client/v4";

/// Cloudflare API client for DNS-01. The token is a secret: no `Debug`/`Serialize`.
pub struct CloudflareDns01 {
    http: reqwest::Client,
    token: String,
    base_url: String,
}

impl CloudflareDns01 {
    pub fn new(token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            token,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Override the API base URL (tests only).
    #[cfg(test)]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Resolve the Cloudflare zone id that owns `fqdn`. Queries `GET /zones?name=`
    /// for each dot-aligned suffix of `fqdn`, most-specific first, returning the
    /// first exact match. Finds the most-specific zone without paginating the
    /// whole account and without a public-suffix-list dependency.
    pub async fn resolve_zone_id(&self, fqdn: &str) -> Result<(String, String), Dns01Error> {
        let fqdn = fqdn.trim_end_matches('.').to_ascii_lowercase();
        for candidate in zone_candidates(&fqdn) {
            let url = format!("{}/zones?name={candidate}", self.base_url);
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| Dns01Error::Http(e.to_string()))?;
            let zones: Vec<CfZone> = cf_result(resp).await?;
            if let Some(zone) = zones
                .into_iter()
                .find(|z| z.name.eq_ignore_ascii_case(&candidate))
            {
                return Ok((zone.id, zone.name));
            }
        }
        Err(Dns01Error::ZoneNotFound(fqdn))
    }

    /// Create a TXT record `name` = `content` in `zone_id`; returns the record id.
    pub async fn create_txt(
        &self,
        zone_id: &str,
        name: &str,
        content: &str,
    ) -> Result<String, Dns01Error> {
        let url = format!("{}/zones/{zone_id}/dns_records", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&txt_create_body(name, content))
            .send()
            .await
            .map_err(|e| Dns01Error::Http(e.to_string()))?;
        let rec: CfDnsRecord = cf_result(resp).await?;
        Ok(rec.id)
    }

    /// Delete a TXT record by id. Best-effort: a missing record is not fatal.
    pub async fn delete_txt(&self, zone_id: &str, record_id: &str) -> Result<(), Dns01Error> {
        let url = format!("{}/zones/{zone_id}/dns_records/{record_id}", self.base_url);
        let resp = self
            .http
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Dns01Error::Http(e.to_string()))?;
        let _: CfDnsRecord = cf_result(resp).await?;
        Ok(())
    }
}

#[async_trait]
impl Dns01Provider for CloudflareDns01 {
    async fn present(&self, fqdn: &str, value: &str) -> Result<TxtHandle, Dns01Error> {
        let (zone_id, _zone_name) = self.resolve_zone_id(fqdn).await?;
        let record_id = self.create_txt(&zone_id, fqdn, value).await?;
        Ok(TxtHandle {
            fqdn: fqdn.to_string(),
            value: value.to_string(),
            provider_ref: Some(format!("{zone_id}:{record_id}")),
        })
    }

    async fn cleanup(&self, handle: &TxtHandle) -> Result<(), Dns01Error> {
        let Some(reference) = handle.provider_ref.as_deref() else {
            return Ok(());
        };
        let Some((zone_id, record_id)) = reference.split_once(':') else {
            return Ok(());
        };
        self.delete_txt(zone_id, record_id).await
    }
}

/// JSON body for a Cloudflare "create TXT record" request. Pure (unit-tested).
fn txt_create_body(name: &str, content: &str) -> serde_json::Value {
    serde_json::json!({ "type": "TXT", "name": name, "content": content, "ttl": 60 })
}

/// Dot-aligned suffixes of `fqdn` with at least two labels, most-specific first
/// (e.g. `x.api.example.com` → `x.api.example.com`, `api.example.com`,
/// `example.com`). A bare TLD is never a Cloudflare zone, so single-label
/// suffixes are skipped. Used to probe `GET /zones?name=` for the owning zone.
pub fn zone_candidates(fqdn: &str) -> Vec<String> {
    let labels: Vec<&str> = fqdn.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n < 2 {
        return Vec::new();
    }
    (0..=(n - 2))
        .map(|start| labels[start..].join("."))
        .collect()
}

/// Classify an unsuccessful Cloudflare API response by status (retryable vs
/// permanent). `errors` never includes the token.
fn classify_api_error(status: u16, errors: String) -> Dns01Error {
    match status {
        401 | 403 => Dns01Error::Auth { status, errors },
        429 | 500..=599 => Dns01Error::Transient { status, errors },
        _ => Dns01Error::Api { status, errors },
    }
}

/// Parse and unwrap a Cloudflare API envelope. Transport/decoding failures and
/// 429/5xx are retryable; 401/403 are auth failures; other 4xx are permanent.
async fn cf_result<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, Dns01Error> {
    let status = resp.status().as_u16();
    let env: CfEnvelope<T> = resp
        .json()
        .await
        .map_err(|e| Dns01Error::Http(e.to_string()))?;
    if !env.success {
        let errors = env
            .errors
            .iter()
            .map(|m| format!("{}:{}", m.code, m.message))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(classify_api_error(status, errors));
    }
    env.result
        .ok_or_else(|| classify_api_error(status, "missing result".to_string()))
}

#[derive(Deserialize)]
struct CfEnvelope<T> {
    success: bool,
    #[serde(default)]
    errors: Vec<CfMessage>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct CfMessage {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct CfZone {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct CfDnsRecord {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    #[test]
    fn zone_candidates_most_specific_first_skips_tld() {
        assert_eq!(
            zone_candidates("x.api.zainokta.com"),
            vec![
                "x.api.zainokta.com".to_string(),
                "api.zainokta.com".to_string(),
                "zainokta.com".to_string(),
            ]
        );
    }

    #[test]
    fn zone_candidates_apex_is_itself() {
        assert_eq!(
            zone_candidates("zainokta.com"),
            vec!["zainokta.com".to_string()]
        );
    }

    #[test]
    fn zone_candidates_single_label_has_none() {
        assert!(zone_candidates("localhost").is_empty());
    }

    #[test]
    fn txt_create_body_has_expected_shape() {
        let body = txt_create_body("_acme-challenge.zainokta.com", "abc123");
        assert_eq!(body["type"], "TXT");
        assert_eq!(body["name"], "_acme-challenge.zainokta.com");
        assert_eq!(body["content"], "abc123");
        assert_eq!(body["ttl"], 60);
    }

    #[tokio::test]
    async fn present_resolves_zone_creates_record_and_returns_handle() {
        let server = MockServer::start_async().await;
        // resolve_zone_id probes most-specific first: the `_acme-challenge.*`
        // candidate misses (not a zone), then the apex hits.
        let miss = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/zones")
                    .query_param("name", "_acme-challenge.zainokta.com");
                then.status(200)
                    .json_body(serde_json::json!({ "success": true, "errors": [], "result": [] }));
            })
            .await;
        let zone = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/zones")
                    .query_param("name", "zainokta.com")
                    .header("authorization", "Bearer secret-token");
                then.status(200).json_body(serde_json::json!({
                    "success": true, "errors": [],
                    "result": [ { "id": "zone123", "name": "zainokta.com" } ]
                }));
            })
            .await;
        let create = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/zones/zone123/dns_records")
                    .header("authorization", "Bearer secret-token");
                then.status(200).json_body(serde_json::json!({
                    "success": true, "errors": [], "result": { "id": "rec456" }
                }));
            })
            .await;

        let cf = CloudflareDns01::new("secret-token".to_string()).with_base_url(server.base_url());
        let handle = cf
            .present("_acme-challenge.zainokta.com", "value")
            .await
            .expect("present");
        assert_eq!(handle.provider_ref.as_deref(), Some("zone123:rec456"));
        miss.assert_async().await;
        zone.assert_async().await;
        create.assert_async().await;

        // cleanup parses the handle and deletes by id.
        let del = server
            .mock_async(|when, then| {
                when.method(DELETE)
                    .path("/zones/zone123/dns_records/rec456");
                then.status(200).json_body(serde_json::json!({
                    "success": true, "errors": [], "result": { "id": "rec456" }
                }));
            })
            .await;
        cf.cleanup(&handle).await.expect("cleanup");
        del.assert_async().await;
    }

    #[tokio::test]
    async fn resolve_zone_id_probes_most_specific_first() {
        let server = MockServer::start_async().await;
        let miss = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/zones")
                    .query_param("name", "x.api.zainokta.com");
                then.status(200).json_body(serde_json::json!({
                    "success": true, "errors": [], "result": []
                }));
            })
            .await;
        let hit = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/zones")
                    .query_param("name", "api.zainokta.com");
                then.status(200).json_body(serde_json::json!({
                    "success": true, "errors": [],
                    "result": [ { "id": "z-sub", "name": "api.zainokta.com" } ]
                }));
            })
            .await;

        let cf = CloudflareDns01::new("t".to_string()).with_base_url(server.base_url());
        let (zone_id, zone_name) = cf
            .resolve_zone_id("x.api.zainokta.com")
            .await
            .expect("resolve_zone_id");
        assert_eq!(zone_id, "z-sub");
        assert_eq!(zone_name, "api.zainokta.com");
        miss.assert_async().await;
        hit.assert_async().await;
    }

    #[tokio::test]
    async fn cf_auth_failure_is_permanent_and_hides_token() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/zones/z/dns_records");
                then.status(403).json_body(serde_json::json!({
                    "success": false,
                    "errors": [ { "code": 10000, "message": "Authentication error" } ],
                    "result": null
                }));
            })
            .await;

        let cf = CloudflareDns01::new("super-secret".to_string()).with_base_url(server.base_url());
        let err = cf
            .create_txt("z", "_acme-challenge.x.com", "v")
            .await
            .expect_err("should fail");
        assert!(matches!(err, Dns01Error::Auth { status: 403, .. }));
        assert!(!err.is_retryable());
        let rendered = format!("{err}");
        assert!(rendered.contains("10000"));
        assert!(!rendered.contains("super-secret"));
    }
}
