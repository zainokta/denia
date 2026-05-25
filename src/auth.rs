use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use rand::RngExt;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::state::{SqliteStore, StateError};

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

pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = random_hex(16);
    let hash = bcrypt_hash(&salt, password);
    Ok(format!("bcrypt:{}:{}", salt, hash))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    if let Some(rest) = hash.strip_prefix("bcrypt:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let salt = parts[0];
            let expected = parts[1];
            let computed = bcrypt_hash(salt, password);
            return expected == computed;
        }
    }
    false
}

fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill(bytes.as_mut_slice());
    hex::encode(&bytes)
}

fn bcrypt_hash(salt: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    hasher.update(b"denia-v1");
    hex::encode(hasher.finalize())
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
    if token == admin_token {
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
}
