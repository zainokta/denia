//! Credential repository trait.

use crate::domain::{Credential, CredentialKind};
use crate::repo::error::RepoError;
use crate::secrets::SecretRef;

#[allow(dead_code)]
pub trait CredentialRepo: Send + Sync + 'static {
    fn put_credential(
        &self,
        name: String,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, RepoError>;
}
