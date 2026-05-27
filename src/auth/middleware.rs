use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::app::AppState;
use crate::repo::sqlite::{SqliteTokenRepo, SqliteUserRepo};

use super::credentials::hash_token;
use super::principal::Principal;

pub(crate) async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|s| s.to_string());

    if let Some(token) = token
        && let Some(principal) = resolve_auth(
            &state.users,
            &state.tokens,
            &token,
            &state.config.admin_token,
        )
    {
        let mut request = request;
        request.extensions_mut().insert(principal);
        return Ok(next.run(request).await);
    }
    Err(StatusCode::UNAUTHORIZED)
}

pub fn resolve_auth(
    users: &SqliteUserRepo,
    tokens: &SqliteTokenRepo,
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
