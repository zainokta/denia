//! Secret material generated during `denia setup` and `denia rotate-token`.
//! Tokens are 32 random bytes -> 64 hex chars (matches FileConfig's >= 64
//! floor in src/config.rs).

use rand::RngExt;

/// Generate a 32-byte random token rendered as 64 lowercase hex chars.
pub fn generate_admin_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_token_is_64_lowercase_hex_chars() {
        let t = generate_admin_token();
        assert_eq!(t.len(), 64);
        assert!(
            t.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "expected lowercase hex, got {t}"
        );
    }

    #[test]
    fn admin_tokens_differ_across_calls() {
        let a = generate_admin_token();
        let b = generate_admin_token();
        assert_ne!(a, b);
    }
}
