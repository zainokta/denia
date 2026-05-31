use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::secrets::SecretRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryAuthKind {
    Anonymous,
    Basic,
    Token,
    EcrToken,
    GarToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub endpoint: String,
    pub auth_kind: RegistryAuthKind,
    pub credential_ref: Option<SecretRef>,
}

impl Registry {
    pub fn new(
        project_id: Uuid,
        name: impl Into<String>,
        endpoint: impl Into<String>,
        auth_kind: RegistryAuthKind,
        credential_ref: Option<SecretRef>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(DomainError::EmptyName);
        }
        let endpoint = endpoint.into();
        let endpoint = endpoint.trim().to_string();
        if endpoint.is_empty() {
            return Err(DomainError::RegistryMissingEndpoint);
        }
        validate_registry_endpoint(&endpoint)?;
        if auth_kind != RegistryAuthKind::Anonymous && credential_ref.is_none() {
            return Err(DomainError::RegistryMissingCredential);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            project_id,
            name,
            endpoint,
            auth_kind,
            credential_ref,
        })
    }
}

pub(crate) fn validate_registry_endpoint(endpoint: &str) -> Result<(), DomainError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(DomainError::RegistryMissingEndpoint);
    }
    if endpoint.contains("://")
        || endpoint.contains('/')
        || endpoint.contains('\\')
        || endpoint.contains('@')
        || endpoint.chars().any(char::is_whitespace)
    {
        return Err(DomainError::InvalidRegistryEndpoint(endpoint.to_string()));
    }
    let host = authority_host(endpoint)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| DomainError::InvalidRegistryEndpoint(endpoint.to_string()))?;
    validate_registry_host(host)
}

pub(crate) fn validate_legacy_image_registry_host(image: &str) -> Result<(), DomainError> {
    if let Some(host) = explicit_registry_host(image.trim()) {
        validate_registry_host(host)?;
    }
    Ok(())
}

fn explicit_registry_host(image: &str) -> Option<&str> {
    let first = image.split('/').next()?;
    if first.eq_ignore_ascii_case("localhost")
        || first.ends_with(".localhost")
        || first.contains('.')
        || first.contains(':')
        || first.starts_with('[')
    {
        authority_host(first)
    } else {
        None
    }
}

fn authority_host(authority: &str) -> Option<&str> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        return (suffix.is_empty() || suffix.starts_with(':')).then_some(host);
    }
    if authority.matches(':').count() == 1 {
        authority.split_once(':').map(|(host, _)| host)
    } else {
        Some(authority)
    }
}

fn validate_registry_host(host: &str) -> Result<(), DomainError> {
    let host = host.trim_end_matches('.');
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
    {
        return Err(DomainError::InvalidRegistryEndpoint(host.to_string()));
    }
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_blocked_registry_ip(ip)
    {
        return Err(DomainError::InvalidRegistryEndpoint(host.to_string()));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(DomainError::InvalidRegistryEndpoint(host.to_string()));
    }
    Ok(())
}

fn is_blocked_registry_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || octets[0] == 0
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_requires_credential_unless_anonymous() {
        let err = Registry::new(
            Uuid::now_v7(),
            "ghcr",
            "ghcr.io",
            RegistryAuthKind::Basic,
            None,
        )
        .unwrap_err();
        assert_eq!(err, DomainError::RegistryMissingCredential);

        assert_eq!(
            Registry::new(
                Uuid::now_v7(),
                "tok",
                "ghcr.io",
                RegistryAuthKind::Token,
                None,
            )
            .unwrap_err(),
            DomainError::RegistryMissingCredential
        );

        let ok = Registry::new(
            Uuid::now_v7(),
            "public",
            "docker.io",
            RegistryAuthKind::Anonymous,
            None,
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn registry_rejects_empty_name_or_endpoint() {
        let p = Uuid::now_v7();
        let r = SecretRef::parse("ghcr-cred").unwrap();
        assert_eq!(
            Registry::new(p, "  ", "ghcr.io", RegistryAuthKind::Basic, Some(r.clone()))
                .unwrap_err(),
            DomainError::EmptyName
        );
        assert_eq!(
            Registry::new(p, "ghcr", "", RegistryAuthKind::Basic, Some(r)).unwrap_err(),
            DomainError::RegistryMissingEndpoint
        );
    }

    #[test]
    fn registry_trims_name_and_endpoint() {
        let reg = Registry::new(
            Uuid::now_v7(),
            "  ghcr  ",
            "  ghcr.io  ",
            RegistryAuthKind::Anonymous,
            None,
        )
        .unwrap();
        assert_eq!(reg.name, "ghcr");
        assert_eq!(reg.endpoint, "ghcr.io");
    }

    #[test]
    fn registry_rejects_local_or_private_endpoints() {
        for endpoint in [
            "localhost:5000",
            "127.0.0.1:5000",
            "10.0.0.10",
            "172.20.0.10",
            "192.168.1.10",
            "169.254.169.254",
            "[::1]:5000",
            "https://ghcr.io",
            "ghcr.io/team",
        ] {
            assert!(
                Registry::new(
                    Uuid::now_v7(),
                    "private",
                    endpoint,
                    RegistryAuthKind::Anonymous,
                    None,
                )
                .is_err(),
                "{endpoint} should be rejected"
            );
        }

        assert!(
            Registry::new(
                Uuid::now_v7(),
                "public",
                "ghcr.io",
                RegistryAuthKind::Anonymous,
                None,
            )
            .is_ok()
        );
    }
}
