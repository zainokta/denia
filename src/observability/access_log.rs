use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEntry {
    /// The store key. NOTE: despite the name, the ingress producer and the
    /// `/v1/services/{id}/requests` reader key this by the **service id**
    /// (`service.id.to_string()`), not the human service name — keying by name
    /// was the cross-project disclosure risk fixed in F-3. The serialized field
    /// name is kept as `service_name` because the web console (area 08) is
    /// aligned to that JSON shape; renaming would break it.
    pub service_name: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub bytes: Option<u64>,
    /// Served latency in milliseconds. Populated by the ingress producer from a
    /// per-request start `Instant`; `None` only for entries built without timing.
    pub duration_ms: Option<u64>,
    pub recorded_at: String,
}

const PER_SERVICE_CAP: usize = 200;

#[derive(Debug, Default, Clone)]
pub struct AccessLogStore {
    inner: Arc<Mutex<std::collections::BTreeMap<String, VecDeque<AccessEntry>>>>,
}

impl AccessLogStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&self, entry: AccessEntry) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let log = guard
            .entry(entry.service_name.clone())
            .or_insert_with(VecDeque::new);
        if log.len() == PER_SERVICE_CAP {
            log.pop_front();
        }
        log.push_back(entry);
    }

    pub fn recent(&self, service_name: &str) -> Vec<AccessEntry> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        guard
            .get(service_name)
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

fn is_uuid_segment(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => is_hex(c),
        })
}

fn is_token_segment(s: &str) -> bool {
    s.len() > 32 && s.chars().all(is_hex)
}

/// Redact likely identifiers/secrets from a request path before it is stored.
/// UUID segments become `{id}` and long (>32-char) hex segments become
/// `{token}`; query strings are stripped by the caller. This is the access-log
/// PII/secret defense, but it is best-effort: a secret embedded as a shorter or
/// non-hex path segment (e.g. a 20-char base64 API key) is NOT detected and
/// passes through verbatim. Operators must therefore treat "do not put secrets
/// in URL path segments" as an assumption (review 07 LOW). Headers and query
/// strings — the usual secret carriers — are never recorded.
pub fn sanitize_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    for segment in path.split('/') {
        if result.is_empty() && segment.is_empty() {
            continue;
        }
        result.push('/');
        if is_uuid_segment(segment) {
            result.push_str("{id}");
        } else if is_token_segment(segment) {
            result.push_str("{token}");
        } else {
            result.push_str(segment);
        }
    }
    if result.is_empty() {
        result.push('/');
    }
    result
}

pub fn parse_request_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let raw_path = parts.next()?.to_string();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    let path = raw_path.split('?').next().unwrap_or(&raw_path).to_string();
    let path = sanitize_path(&path);
    Some((method, path))
}

pub fn parse_status_line(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_line() {
        let (method, path) = parse_request_line("GET /healthz HTTP/1.1").expect("parsed");
        assert_eq!(method, "GET");
        assert_eq!(path, "/healthz");
    }

    #[test]
    fn parse_request_line_sanitizes_uuid() {
        let (method, path) = parse_request_line(
            "GET /api/users/a1b2c3d4-e5f6-7890-abcd-ef1234567890/profile HTTP/1.1",
        )
        .expect("parsed");
        assert_eq!(method, "GET");
        assert_eq!(path, "/api/users/{id}/profile");
    }

    #[test]
    fn sanitize_path_replaces_uuids_and_tokens() {
        assert_eq!(sanitize_path("/healthz"), "/healthz");
        assert_eq!(
            sanitize_path("/api/users/a1b2c3d4-e5f6-7890-abcd-ef1234567890/profile"),
            "/api/users/{id}/profile"
        );
        assert_eq!(
            sanitize_path("/download/abcdef0123456789abcdef0123456789abcdef/file"),
            "/download/{token}/file"
        );
        assert_eq!(
            sanitize_path(
                "/a/a1b2c3d4-e5f6-7890-abcd-ef1234567890/b/abcdef0123456789abcdef0123456789abcdef/c"
            ),
            "/a/{id}/b/{token}/c"
        );
        assert_eq!(
            sanitize_path("/static/abcdef0123456789abcdef0123456789abcdef"),
            "/static/{token}"
        );
    }

    #[test]
    fn rejects_non_http() {
        assert!(parse_request_line("hello world other").is_none());
    }

    #[test]
    fn parses_status_line() {
        assert_eq!(parse_status_line("HTTP/1.1 204 No Content"), Some(204));
    }

    #[test]
    fn stores_per_service_recent_newest_first() {
        let store = AccessLogStore::new();
        store.append(AccessEntry {
            service_name: "web".into(),
            method: "GET".into(),
            path: "/a".into(),
            status: 200,
            bytes: None,
            duration_ms: None,
            recorded_at: "t1".into(),
        });
        store.append(AccessEntry {
            service_name: "web".into(),
            method: "GET".into(),
            path: "/b".into(),
            status: 500,
            bytes: None,
            duration_ms: None,
            recorded_at: "t2".into(),
        });
        let entries = store.recent("web");
        assert_eq!(entries[0].path, "/b");
        assert_eq!(entries[1].path, "/a");
    }

    #[test]
    fn caps_to_two_hundred() {
        let store = AccessLogStore::new();
        for i in 0..210 {
            store.append(AccessEntry {
                service_name: "web".into(),
                method: "GET".into(),
                path: format!("/{i}"),
                status: 200,
                bytes: None,
                duration_ms: None,
                recorded_at: i.to_string(),
            });
        }
        let entries = store.recent("web");
        assert_eq!(entries.len(), 200);
        assert_eq!(entries[0].path, "/209");
    }
}
