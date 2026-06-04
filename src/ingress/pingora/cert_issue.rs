//! On-demand TLS certificate issuance request channel.
//!
//! The ACME issuance/renewal task in `daemon.rs` runs the initial issuance pass
//! at boot and then only re-scans every 12 hours. That left a window where a
//! `tls_enabled` domain that was verified (or whose service was first deployed)
//! after boot had a `:443` route but no certificate — so every HTTPS handshake
//! to that host was declined for up to ~12h (review HIGH).
//!
//! This is the "issue requested" channel the review recommends: the verify path
//! ([`crate::api::domains`]) and the deploy-completion path
//! ([`crate::api::deployments`]) push a hostname onto an unbounded MPSC queue
//! when a TLS-enabled domain becomes live, and the ACME task drains it and
//! issues immediately (within seconds, not ~12h).
//!
//! The control plane only ever holds the [`CertIssueSender`]; the daemon takes
//! the single [`CertIssueReceiver`] out of [`AppState`] once when it spawns the
//! ACME task. When no ACME driver is configured (no email / TLS off) the daemon
//! never takes the receiver, so the sender's pushes are simply dropped — issuing
//! requires a driver anyway.

use tokio::sync::mpsc;

/// Cloneable sender for on-demand cert issuance requests (validated, lowercased
/// hostnames). Held by [`crate::app::AppState`]; cloned freely into request
/// handlers and the deploy task.
pub type CertIssueSender = mpsc::UnboundedSender<String>;

/// Receiver drained by the daemon's ACME task. Taken out of `AppState` exactly
/// once at boot (the channel is single-consumer).
pub type CertIssueReceiver = mpsc::UnboundedReceiver<String>;

/// Build a fresh on-demand issuance channel `(sender, receiver)`.
pub fn channel() -> (CertIssueSender, CertIssueReceiver) {
    mpsc::unbounded_channel()
}

/// Best-effort request issuance for `hostname`. Validation/lowercasing is the
/// caller's job (or the ACME task re-validates via `AcmeDriver::issue`); a send
/// failure means no consumer is attached (TLS disabled or daemon shutting down)
/// and is silently ignored — issuance is best-effort and the 12h renewal scan
/// remains a backstop.
pub fn request_issue(sender: &CertIssueSender, hostname: impl Into<String>) {
    let _ = sender.send(hostname.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_issue_delivers_hostname_to_receiver() {
        let (tx, mut rx) = channel();
        request_issue(&tx, "api.example.com");
        request_issue(&tx, "www.example.com".to_string());
        assert_eq!(rx.recv().await.as_deref(), Some("api.example.com"));
        assert_eq!(rx.recv().await.as_deref(), Some("www.example.com"));
    }

    #[tokio::test]
    async fn request_issue_is_noop_when_receiver_dropped() {
        let (tx, rx) = channel();
        drop(rx);
        // No panic / no error surfaced to the caller; send is best-effort.
        request_issue(&tx, "api.example.com");
    }
}
