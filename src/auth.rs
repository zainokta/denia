use argon2::{
    Argon2, ParamsBuilder, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::SaltString,
};
use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use rand::RngExt;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

use crate::state::{SqliteStore, StateError};

const ARGON2_MEMORY: u32 = 19456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("token not found")]
    InvalidToken,
    #[error("insufficient permissions")]
    Forbidden,
    #[error("state error: {0}")]
    State(#[from] StateError),
}

fn argon2_hasher() -> Argon2<'static> {
    let params = ParamsBuilder::new()
        .m_cost(ARGON2_MEMORY)
        .t_cost(ARGON2_ITERATIONS)
        .p_cost(ARGON2_PARALLELISM)
        .build()
        .expect("valid argon2 params");
    Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
}

pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| AuthError::InvalidCredentials)?;
    let hasher = argon2_hasher();
    let hash = hasher
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthError::InvalidCredentials)?
        .to_string();
    Ok(format!("argon2id:{}", hash))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    if let Some(rest) = hash.strip_prefix("argon2id:") {
        let parsed = PasswordHash::new(rest);
        match parsed {
            Ok(parsed_hash) => argon2_hasher()
                .verify_password(password.as_bytes(), &parsed_hash)
                .is_ok(),
            Err(_) => false,
        }
    } else {
        false
    }
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Option<Uuid>,
    pub is_super_admin: bool,
}

impl Principal {
    pub fn super_admin() -> Self {
        Self {
            user_id: None,
            is_super_admin: true,
        }
    }

    pub fn user(user_id: Uuid, is_super_admin: bool) -> Self {
        Self {
            user_id: Some(user_id),
            is_super_admin,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
}

impl<S> FromRequestParts<S> for Principal
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

pub fn resolve_auth(store: &SqliteStore, token: &str, admin_token: &str) -> Option<Principal> {
    if token.len() == admin_token.len()
        && token.as_bytes().ct_eq(admin_token.as_bytes()).unwrap_u8() == 1
    {
        return Some(Principal::super_admin());
    }
    let token_hash = hash_token(token);
    if let Ok(Some(user)) = store.user_for_session(&token_hash) {
        return Some(Principal::user(user.id, user.is_super_admin));
    }
    if let Ok(Some(user)) = store.user_for_api_token(&token_hash) {
        return Some(Principal::user(user.id, user.is_super_admin));
    }
    None
}

pub fn require_project_role(
    principal: &Principal,
    project_role: Option<crate::domain::Role>,
    required_min: crate::domain::Role,
) -> Result<(), AuthError> {
    if principal.is_super_admin {
        return Ok(());
    }
    let role = project_role.ok_or(AuthError::Forbidden)?;
    if role >= required_min {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_round_trip() {
        let hash = hash_password("secret123").unwrap();
        assert!(verify_password(&hash, "secret123"));
        assert!(!verify_password(&hash, "wrong"));
    }

    #[test]
    fn hash_token_is_stable() {
        let token = generate_token();
        let hashed = hash_token(&token);
        assert_eq!(hashed.len(), 64);
        assert_eq!(hashed, hash_token(&token));
    }

    #[test]
    fn require_project_role_super_admin_bypasses_role_check() {
        let principal = Principal::super_admin();
        assert!(require_project_role(&principal, None, crate::domain::Role::Admin).is_ok());
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Viewer),
                crate::domain::Role::Admin
            )
            .is_ok()
        );
    }

    #[test]
    fn require_project_role_forbids_lower_role() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(matches!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Operator),
                crate::domain::Role::Admin
            ),
            Err(AuthError::Forbidden)
        ));
        assert!(matches!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Viewer),
                crate::domain::Role::Operator
            ),
            Err(AuthError::Forbidden)
        ));
    }

    #[test]
    fn require_project_role_allows_equal_or_higher_role() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Admin),
                crate::domain::Role::Admin
            )
            .is_ok()
        );
        assert!(
            require_project_role(
                &principal,
                Some(crate::domain::Role::Admin),
                crate::domain::Role::Operator
            )
            .is_ok()
        );
    }

    #[test]
    fn require_project_role_forbids_non_member() {
        let principal = Principal::user(uuid::Uuid::now_v7(), false);
        assert!(matches!(
            require_project_role(&principal, None, crate::domain::Role::Viewer),
            Err(AuthError::Forbidden)
        ));
    }
}
