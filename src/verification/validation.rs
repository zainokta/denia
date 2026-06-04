use rand::Rng;

use crate::verification::error::HostnameError;

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

    /// Review LOW — two hostname validators coexist: `validate_hostname`
    /// (storage / domain CRUD) and `ingress::validate_domain` (routing / SNI /
    /// ACME). The security surface is covered because routing re-validates, but
    /// a divergence could let a domain be STORED that can never ROUTE. The
    /// load-bearing invariant is one-directional: every hostname accepted by
    /// `validate_hostname` MUST also be accepted by `validate_domain` (the
    /// reverse is not required — routing may be more permissive). This asserts
    /// that direction over a representative corpus so a future tightening of
    /// `validate_domain` that breaks it fails the build instead of silently
    /// stranding stored domains.
    #[test]
    fn validate_hostname_accepted_implies_validate_domain_accepted() {
        use crate::ingress::pingora::state::validate_domain;
        let corpus = [
            "app.example.com",
            "a.b.co",
            "api-1.svc.example.com",
            "xn--mnchen-3ya.de",
            "a.example.com",
            // long-but-valid label (63 chars) + tld
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.example.com",
            // uppercase: validate_hostname rejects it, so it never reaches here,
            // but include the lowercased form it would store.
            "deep.nested.sub.domain.example.com",
            // invalid inputs (rejected by validate_hostname) are skipped below.
            "APP.example.com",
            "localhost",
            "127.0.0.1",
            "example.com:8080",
            "ev`il.com",
        ];
        for input in corpus {
            if let Ok(stored) = validate_hostname(input) {
                assert!(
                    validate_domain(&stored).is_ok(),
                    "validate_hostname accepted {input:?} (stored as {stored:?}) but \
                     validate_domain rejected it — a stored domain that can never route"
                );
            }
        }
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
