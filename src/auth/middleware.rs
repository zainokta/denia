use subtle::ConstantTimeEq;

use crate::repo::{TokenRepo, UserRepo};

use super::credentials::hash_token;
use super::principal::Principal;

pub fn resolve_auth(
    users: &dyn UserRepo,
    tokens: &dyn TokenRepo,
    token: &str,
    admin_token: &str,
) -> Option<Principal> {
    if token.len() == admin_token.len()
        && token.as_bytes().ct_eq(admin_token.as_bytes()).unwrap_u8() == 1
    {
        return Some(Principal::super_admin());
    }
    let token_hash = hash_token(token);
    if let Ok(Some(user)) = users.user_for_session(&token_hash) {
        return Some(Principal::user(user.id, user.is_super_admin));
    }
    if let Ok(Some(user)) = tokens.user_for_api_token(&token_hash) {
        return Some(Principal::user(user.id, user.is_super_admin));
    }
    None
}
