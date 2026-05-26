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
}
