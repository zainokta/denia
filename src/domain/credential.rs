use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::secrets::SecretRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    SshDeployKey,
    RegistryBasic,
    RegistryToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub kind: CredentialKind,
    pub secret_ref: SecretRef,
}
