use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEntry {
    pub service_name: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub bytes: Option<u64>,
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

pub fn parse_request_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
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
