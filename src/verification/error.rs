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
