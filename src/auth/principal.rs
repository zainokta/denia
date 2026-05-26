use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use uuid::Uuid;

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
