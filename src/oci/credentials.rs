use oci_client::secrets::RegistryAuth;

use super::OciError;
use crate::domain::RegistryAuthKind;
use crate::secrets::SecretPayload;

fn payload_value(payload: Option<&SecretPayload>) -> Result<String, OciError> {
    payload
        .map(|p| p.value.clone())
        .ok_or_else(|| OciError::Pull("registry credential is required".into()))
}

pub fn resolve_registry_auth(
    kind: RegistryAuthKind,
    payload: Option<&SecretPayload>,
) -> Result<RegistryAuth, OciError> {
    match kind {
        RegistryAuthKind::Anonymous => Ok(RegistryAuth::Anonymous),
        RegistryAuthKind::Basic => {
            let raw =
                payload.ok_or_else(|| OciError::Pull("registry credential is required".into()))?;
            let (user, pass) = raw
                .value
                .split_once(':')
                .ok_or_else(|| OciError::Pull("basic credential must be 'user:password'".into()))?;
            Ok(RegistryAuth::Basic(user.to_string(), pass.to_string()))
        }
        RegistryAuthKind::Token => Ok(RegistryAuth::Bearer(payload_value(payload)?)),
        RegistryAuthKind::EcrToken => {
            Ok(RegistryAuth::Basic("AWS".into(), payload_value(payload)?))
        }
        RegistryAuthKind::GarToken => Ok(RegistryAuth::Basic(
            "oauth2accesstoken".into(),
            payload_value(payload)?,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RegistryAuthKind;
    use crate::secrets::SecretPayload;
    use oci_client::secrets::RegistryAuth;

    #[test]
    fn anonymous_needs_no_payload() {
        let auth = resolve_registry_auth(RegistryAuthKind::Anonymous, None).unwrap();
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }

    #[test]
    fn basic_splits_user_password() {
        let p = SecretPayload::new("alice:s3cret");
        match resolve_registry_auth(RegistryAuthKind::Basic, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => {
                assert_eq!(u, "alice");
                assert_eq!(pw, "s3cret");
            }
            _ => panic!("expected basic"),
        }
    }

    #[test]
    fn basic_rejects_malformed_payload() {
        let p = SecretPayload::new("no-colon");
        assert!(resolve_registry_auth(RegistryAuthKind::Basic, Some(&p)).is_err());
    }

    #[test]
    fn basic_requires_payload() {
        assert!(resolve_registry_auth(RegistryAuthKind::Basic, None).is_err());
    }

    #[test]
    fn token_maps_to_bearer() {
        let p = SecretPayload::new("abc123");
        match resolve_registry_auth(RegistryAuthKind::Token, Some(&p)).unwrap() {
            RegistryAuth::Bearer(t) => assert_eq!(t, "abc123"),
            _ => panic!("expected bearer"),
        }
    }

    #[test]
    fn token_requires_payload() {
        assert!(resolve_registry_auth(RegistryAuthKind::Token, None).is_err());
    }

    #[test]
    fn ecr_and_gar_map_to_fixed_users() {
        let p = SecretPayload::new("tok");
        match resolve_registry_auth(RegistryAuthKind::EcrToken, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => {
                assert_eq!(u, "AWS");
                assert_eq!(pw, "tok");
            }
            _ => panic!(),
        }
        match resolve_registry_auth(RegistryAuthKind::GarToken, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => {
                assert_eq!(u, "oauth2accesstoken");
                assert_eq!(pw, "tok");
            }
            _ => panic!(),
        }
    }
}
