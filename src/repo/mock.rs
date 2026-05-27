//! Test-only fakes for non-repo seams.
//!
//! Gated behind `cfg(test)` (for this crate's own unit tests) or the
//! `test-support` cargo feature (for downstream integration tests). They must
//! never compile into the release binary.
//!
//! Repositories are concrete (`Sqlite*Repo` backed by an in-memory SQLite pool
//! in tests), so no repo mocks live here — only seams whose real implementation
//! has unfakeable side effects.
#![cfg(any(test, feature = "test-support"))]

use crate::verification::{DomainVerifier, error::DomainVerifyError};

// --- Domain verifier stub -------------------------------------------------

/// A `DomainVerifier` that always succeeds, for handler tests that exercise the
/// verify path without making an outbound HTTP request.
#[derive(Default)]
pub struct StubDomainVerifier;

#[async_trait::async_trait]
impl DomainVerifier for StubDomainVerifier {
    async fn verify(&self, _hostname: &str, _token: &str) -> Result<(), DomainVerifyError> {
        Ok(())
    }
}
