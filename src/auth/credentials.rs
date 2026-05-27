use argon2::{
    Argon2, ParamsBuilder, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::SaltString,
};
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use thiserror::Error;

use crate::state::StateError;

const ARGON2_MEMORY: u32 = 47104;
const ARGON2_ITERATIONS: u32 = 3;
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

/// Precomputed Argon2id hash used to equalize login timing when a username does
/// not exist, preventing user enumeration via response-time side channel.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("denia-timing-equalizer").expect("dummy hash"));

/// Run an Argon2id verification against a constant hash and discard the result.
/// Callers invoke this on the "user not found" path so its timing matches the
/// "user found, wrong password" path.
pub fn verify_dummy_password() {
    let _ = verify_password(&DUMMY_HASH, "wrong-password");
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
