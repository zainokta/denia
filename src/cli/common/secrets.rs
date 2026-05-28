//! Secret material generated during `denia setup` and `denia rotate-token`.
//! Tokens are 32 random bytes -> 64 hex chars (matches FileConfig's >= 64
//! floor in src/config.rs).

use age::secrecy::ExposeSecret as _;
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

/// Generate a fresh X25519 age identity and return it serialised in the exact
/// format that `age-keygen` writes:
///
/// ```text
/// # public key: age1<base32>
/// AGE-SECRET-KEY-1<base32>
/// ```
///
/// The format matters because [`crate::config::read_age_public_key`] parses
/// the `# public key:` comment to auto-derive `DENIA_AGE_RECIPIENT` when the
/// operator does not set it explicitly (see ADR-021 / ADR-023).  Callers MUST
/// persist this string only via an atomic `0640` write; never log it or expose
/// it in error messages.
pub fn generate_age_identity() -> String {
    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public();
    let secret = identity.to_string();
    format!("# public key: {recipient}\n{}\n", secret.expose_secret())
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

    #[test]
    fn age_identity_has_public_key_comment() {
        let s = generate_age_identity();
        assert!(s.contains("# public key: age1"), "missing public key comment in:\n{s}");
        assert!(s.contains("AGE-SECRET-KEY-1"), "missing secret body in:\n{s}");
    }

    #[test]
    fn age_identity_parses_back() {
        let s = generate_age_identity();
        // Find the AGE-SECRET-KEY-1 line and parse it back as an x25519 identity;
        // round-trip the recipient and confirm it matches the comment.
        let secret_line = s
            .lines()
            .find(|l| l.starts_with("AGE-SECRET-KEY-1"))
            .expect("secret body present");
        let parsed: age::x25519::Identity = secret_line
            .parse()
            .expect("identity parses");
        let expected_comment = format!("# public key: {}", parsed.to_public());
        assert!(
            s.lines().any(|l| l == expected_comment),
            "round-trip recipient mismatch: looking for {expected_comment:?} in:\n{s}"
        );
    }
}
