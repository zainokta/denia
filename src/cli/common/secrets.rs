//! Secret material generated during `denia setup` and `denia rotate-token`.
//! Tokens are 32 random bytes -> 64 hex chars (matches FileConfig's >= 64
//! floor in src/config.rs).

use rand::RngExt;

/// Generate a 32-byte random admin token (256 bits, OS-backed CSPRNG via
/// `rand::rng()`) rendered as 64 lowercase hex chars.
///
/// The returned value is plaintext and grants super-admin access to the
/// `/v1` API. Callers MUST:
/// - Persist it only via an atomic `0640` write under the operator's
///   `~/.config/denia/admin.token`.
/// - Never log, print to non-interactive output, or include it in error
///   messages.
/// - Match the `>= 64 hex char` floor enforced by `AppConfig::from_env`
///   (see `src/config.rs`).
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
    fn admin_tokens_are_unique_across_many_calls() {
        // Ten draws from a 256-bit CSPRNG: probability of any collision is
        // astronomically small. This catches the catastrophic failure mode
        // where the generator returns a constant or a short cycle.
        let tokens: std::collections::HashSet<String> =
            (0..10).map(|_| generate_admin_token()).collect();
        assert_eq!(tokens.len(), 10, "expected 10 distinct tokens, got duplicates");
    }
}
