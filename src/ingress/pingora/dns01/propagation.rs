//! Provider-agnostic DNS-01 propagation: CNAME delegation resolution + a
//! DoH-based "is the TXT visible yet" check. Run by the solver for every provider
//! (lego keeps propagation generic; providers only tune `Timeout()`).
//!
//! CNAME delegation: if `_acme-challenge.<domain>` is a CNAME to another name,
//! the TXT must be created/verified at the CNAME target (the "effective FQDN").
//! This lets a supported provider (e.g. a Cloudflare token) answer challenges for
//! a domain whose apex DNS lives elsewhere — delegate the `_acme-challenge` record
//! into a zone you control.

use std::collections::HashSet;
use std::time::Duration;

use serde::Deserialize;

/// Built-in DoH resolvers used when the operator configures none. Public JSON
/// DoH endpoints (Cloudflare + Google).
const DEFAULT_DOH_RESOLVERS: &[&str] = &[
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
];

/// Maximum CNAME hops followed when resolving the effective challenge FQDN.
const MAX_CNAME_HOPS: usize = 8;

/// Per-request DoH timeout so a hung resolver cannot eat the propagation budget.
const DOH_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// DNS record type for TXT (RFC 1035).
const DNS_TYPE_TXT: u16 = 16;
/// DNS record type for CNAME.
const DNS_TYPE_CNAME: u16 = 5;

use super::Dns01Error;

/// Confirms DNS-01 record propagation and resolves CNAME delegation. Built from
/// config (`delay`, `resolvers`) plus the provider's `timeout()`.
pub struct PropagationCheck {
    http: reqwest::Client,
    /// Initial wait before the first visibility poll (`acme_dns_propagation_secs`).
    delay: Duration,
    /// Overall polling budget after the delay (provider `timeout().0`).
    timeout: Duration,
    /// Interval between polls (provider `timeout().1`).
    interval: Duration,
    resolvers: Vec<String>,
}

impl PropagationCheck {
    pub fn new(
        delay: Duration,
        timeout: Duration,
        interval: Duration,
        resolvers: Vec<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(DOH_REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            http,
            delay,
            timeout,
            interval,
            resolvers,
        }
    }

    fn effective_resolvers(&self) -> Vec<&str> {
        if self.resolvers.is_empty() {
            DEFAULT_DOH_RESOLVERS.to_vec()
        } else {
            self.resolvers.iter().map(String::as_str).collect()
        }
    }

    /// Resolve CNAME delegation for the challenge record name. Follows a CNAME
    /// chain (up to [`MAX_CNAME_HOPS`]) and returns the final target — the FQDN
    /// where the TXT record must actually live — or the input unchanged when a
    /// resolver answers that there is no CNAME. Errors (rather than silently using
    /// the input) when a loop or over-long chain is detected, or when **every**
    /// resolver fails to answer: presenting at a possibly-delegated owner name
    /// would silently fail validation, so refuse instead.
    pub async fn resolve_cname(&self, fqdn: &str) -> Result<String, Dns01Error> {
        let resolvers = self.effective_resolvers();
        let mut current = fqdn.trim_end_matches('.').to_string();
        let mut visited = HashSet::new();
        visited.insert(current.clone());
        for _ in 0..MAX_CNAME_HOPS {
            let mut next = None;
            let mut answered = false;
            let mut last_err = None;
            for resolver in &resolvers {
                match self.lookup_cname(resolver, &current).await {
                    Ok(Some(target)) => {
                        next = Some(target.trim_end_matches('.').to_string());
                        answered = true;
                        break;
                    }
                    Ok(None) => answered = true, // resolver answered: no CNAME here
                    Err(e) => last_err = Some(e),
                }
            }
            match next {
                Some(target) if target == current => return Ok(current),
                Some(target) => {
                    if !visited.insert(target.clone()) {
                        return Err(Dns01Error::Cname(format!(
                            "loop resolving {fqdn} at {target}"
                        )));
                    }
                    current = target;
                }
                // No CNAME found. Only treat as "this is the owner name" if a
                // resolver actually answered; if every resolver errored, surface
                // the (retryable) lookup failure rather than presenting at a name
                // that may in fact be delegated.
                None if answered => return Ok(current),
                None => {
                    return Err(last_err.unwrap_or_else(|| {
                        Dns01Error::Cname(format!("CNAME lookup failed for {fqdn}"))
                    }));
                }
            }
        }
        Err(Dns01Error::Cname(format!(
            "chain exceeds {MAX_CNAME_HOPS} hops for {fqdn}"
        )))
    }

    /// Wait until the TXT record `fqdn` = `value` is visible on a resolver. Sleeps
    /// `delay`, then polls every `interval` until seen or `timeout` elapses, so
    /// the CA is never asked to validate an unpropagated record.
    pub async fn await_txt(&self, fqdn: &str, value: &str) -> Result<(), Dns01Error> {
        tokio::time::sleep(self.delay).await;
        let resolvers = self.effective_resolvers();
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            for resolver in &resolvers {
                if self.lookup_has_txt(resolver, fqdn, value).await {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Dns01Error::Propagation(fqdn.to_string()));
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    /// Query a DoH endpoint for a CNAME on `name`. `Ok(Some(target))` if a CNAME
    /// answer is present, `Ok(None)` if the resolver answered with no CNAME, and
    /// `Err` on a transport/parse failure (so a failed lookup is NOT mistaken for
    /// "no delegation" by [`resolve_cname`]).
    async fn lookup_cname(&self, resolver: &str, name: &str) -> Result<Option<String>, Dns01Error> {
        let body = self.doh_query(resolver, name, "CNAME").await?;
        Ok(body
            .answer
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.record_type == Some(DNS_TYPE_CNAME))
            .map(|a| a.data.trim_matches('"').to_string()))
    }

    /// `true` if a TXT (type 16) answer for `name` equals `expected`. A lookup
    /// failure returns `false` (the caller polls until `timeout`), unlike the
    /// one-shot CNAME resolution which must surface lookup errors.
    async fn lookup_has_txt(&self, resolver: &str, name: &str, expected: &str) -> bool {
        let Ok(body) = self.doh_query(resolver, name, "TXT").await else {
            return false;
        };
        body.answer
            .unwrap_or_default()
            .iter()
            .filter(|a| a.record_type == Some(DNS_TYPE_TXT))
            .any(|a| a.data.trim_matches('"') == expected)
    }

    async fn doh_query(
        &self,
        resolver: &str,
        name: &str,
        rtype: &str,
    ) -> Result<DohResponse, Dns01Error> {
        let url = format!("{resolver}?name={name}&type={rtype}");
        let resp = self
            .http
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
            .map_err(|e| Dns01Error::Http(e.to_string()))?
            // Reject HTTP 4xx/5xx (incl. 429/5xx) so a non-2xx body is never
            // parsed as a definitive "no record" answer.
            .error_for_status()
            .map_err(|e| Dns01Error::Http(e.to_string()))?;
        let body: DohResponse = resp
            .json()
            .await
            .map_err(|e| Dns01Error::Http(e.to_string()))?;
        // DoH `Status` (RFC 8484 / RCODE): 0 NOERROR and 3 NXDOMAIN are definitive
        // answers (NXDOMAIN = the `_acme-challenge` name simply has no CNAME).
        // Any other non-zero code (2 SERVFAIL, 5 REFUSED, …) is a transient lookup
        // failure that must NOT be read as "no record".
        if let Some(status) = body.status
            && status != 0
            && status != 3
        {
            return Err(Dns01Error::Http(format!(
                "resolver {resolver} returned DNS status {status} for {name}"
            )));
        }
        Ok(body)
    }
}

#[derive(Deserialize)]
struct DohResponse {
    #[serde(rename = "Status")]
    status: Option<u16>,
    #[serde(rename = "Answer")]
    answer: Option<Vec<DohAnswer>>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    record_type: Option<u16>,
    data: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn check(server: &MockServer) -> PropagationCheck {
        PropagationCheck::new(
            Duration::from_secs(0),
            Duration::from_secs(2),
            Duration::from_millis(10),
            vec![format!("{}/dns-query", server.base_url())],
        )
    }

    #[tokio::test]
    async fn resolve_cname_follows_delegation() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/dns-query")
                    .query_param("name", "_acme-challenge.zainokta.com")
                    .query_param("type", "CNAME");
                then.status(200).json_body(serde_json::json!({
                    "Answer": [ { "type": 5, "data": "_acme-challenge.delegated.example." } ]
                }));
            })
            .await;
        // The delegated target has no further CNAME (empty answer).
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .query_param("name", "_acme-challenge.delegated.example")
                    .query_param("type", "CNAME");
                then.status(200)
                    .json_body(serde_json::json!({ "Answer": [] }));
            })
            .await;

        let p = check(&server);
        let effective = p
            .resolve_cname("_acme-challenge.zainokta.com")
            .await
            .expect("resolve");
        assert_eq!(effective, "_acme-challenge.delegated.example");
    }

    #[tokio::test]
    async fn resolve_cname_no_record_returns_input() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).query_param("type", "CNAME");
                then.status(200)
                    .json_body(serde_json::json!({ "Answer": [] }));
            })
            .await;
        let p = check(&server);
        assert_eq!(
            p.resolve_cname("_acme-challenge.zainokta.com")
                .await
                .expect("resolve"),
            "_acme-challenge.zainokta.com"
        );
    }

    #[tokio::test]
    async fn resolve_cname_loop_is_hard_error() {
        let server = MockServer::start_async().await;
        // a -> b and b -> a: a loop must error, not silently proceed.
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .query_param("name", "a.example")
                    .query_param("type", "CNAME");
                then.status(200).json_body(
                    serde_json::json!({ "Answer": [ { "type": 5, "data": "b.example." } ] }),
                );
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .query_param("name", "b.example")
                    .query_param("type", "CNAME");
                then.status(200).json_body(
                    serde_json::json!({ "Answer": [ { "type": 5, "data": "a.example." } ] }),
                );
            })
            .await;
        let p = check(&server);
        let err = p.resolve_cname("a.example").await.expect_err("loop");
        assert!(matches!(err, Dns01Error::Cname(_)));
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn resolve_cname_servfail_is_error_not_no_cname() {
        // A DoH SERVFAIL (Status 2) with an empty Answer must NOT be read as
        // "no CNAME" — it is a transient lookup failure.
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).query_param("type", "CNAME");
                then.status(200)
                    .json_body(serde_json::json!({ "Status": 2, "Answer": [] }));
            })
            .await;
        let p = check(&server);
        let err = p
            .resolve_cname("_acme-challenge.zainokta.com")
            .await
            .expect_err("servfail");
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn resolve_cname_nxdomain_is_no_cname() {
        // NXDOMAIN (Status 3) for the CNAME query means the name has no CNAME
        // (common: `_acme-challenge` does not exist yet) → terminal owner name.
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).query_param("type", "CNAME");
                then.status(200)
                    .json_body(serde_json::json!({ "Status": 3, "Answer": [] }));
            })
            .await;
        let p = check(&server);
        assert_eq!(
            p.resolve_cname("_acme-challenge.zainokta.com")
                .await
                .expect("nxdomain = no cname"),
            "_acme-challenge.zainokta.com"
        );
    }

    #[tokio::test]
    async fn resolve_cname_all_resolvers_failing_is_error() {
        // Unreachable resolver (connection refused) → every DoH request errors →
        // resolve_cname must surface the (retryable) error, NOT silently return
        // the input name (which could in fact be delegated).
        let p = PropagationCheck::new(
            Duration::from_secs(0),
            Duration::from_secs(1),
            Duration::from_millis(10),
            vec!["http://127.0.0.1:1/dns-query".to_string()],
        );
        let err = p
            .resolve_cname("_acme-challenge.zainokta.com")
            .await
            .expect_err("all resolvers failed");
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn await_txt_returns_once_value_visible() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .query_param("name", "_acme-challenge.zainokta.com")
                    .query_param("type", "TXT");
                then.status(200).json_body(serde_json::json!({
                    "Answer": [ { "type": 16, "data": "\"expected-value\"" } ]
                }));
            })
            .await;
        let p = check(&server);
        p.await_txt("_acme-challenge.zainokta.com", "expected-value")
            .await
            .expect("should observe the TXT");
    }

    #[tokio::test]
    async fn await_txt_times_out_when_absent() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).query_param("type", "TXT");
                then.status(200)
                    .json_body(serde_json::json!({ "Answer": [] }));
            })
            .await;
        let p = check(&server);
        let err = p
            .await_txt("_acme-challenge.zainokta.com", "expected-value")
            .await
            .expect_err("should time out");
        assert!(matches!(err, Dns01Error::Propagation(_)));
    }
}
