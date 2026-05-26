use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::{
    deploy::DeployError, metrics::MetricsError, node_metrics::NodeMetricsError, repo::RepoError,
};

#[derive(Debug)]
pub enum ApiError {
    State(crate::state::StateError),
    Repo(RepoError),
    Auth(crate::auth::AuthError),
    InvalidSecretRef(crate::secrets::SecretRefError),
    BadRequest(String),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    Deploy(DeployError),
    Log(std::io::Error),
    Metrics(MetricsError),
    NodeMetrics(NodeMetricsError),
}

impl From<crate::auth::AuthError> for ApiError {
    fn from(value: crate::auth::AuthError) -> Self {
        match value {
            crate::auth::AuthError::InvalidCredentials => {
                ApiError::Unauthorized("invalid credentials".to_string())
            }
            crate::auth::AuthError::Forbidden => ApiError::Forbidden("forbidden".to_string()),
            crate::auth::AuthError::InvalidToken => {
                ApiError::Unauthorized("invalid token".to_string())
            }
            crate::auth::AuthError::State(e) => ApiError::State(e),
        }
    }
}

impl From<crate::state::StateError> for ApiError {
    fn from(value: crate::state::StateError) -> Self {
        Self::State(value)
    }
}

impl From<RepoError> for ApiError {
    fn from(value: RepoError) -> Self {
        Self::Repo(value)
    }
}

impl From<DeployError> for ApiError {
    fn from(value: DeployError) -> Self {
        Self::Deploy(value)
    }
}

impl From<MetricsError> for ApiError {
    fn from(value: MetricsError) -> Self {
        Self::Metrics(value)
    }
}

impl From<NodeMetricsError> for ApiError {
    fn from(value: NodeMetricsError) -> Self {
        Self::NodeMetrics(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::State(error) => match &error {
                crate::state::StateError::ProjectNotEmpty => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                crate::state::StateError::UnknownProject => {
                    (StatusCode::NOT_FOUND, error.to_string())
                }
                crate::state::StateError::RegistryNotFound => {
                    (StatusCode::NOT_FOUND, error.to_string())
                }
                crate::state::StateError::RegistryInUse => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                crate::state::StateError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::state::StateError::LastSuperAdmin => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Repo(error) => match &error {
                RepoError::ProjectNotEmpty => (StatusCode::CONFLICT, error.to_string()),
                RepoError::UnknownProject => (StatusCode::NOT_FOUND, error.to_string()),
                RepoError::RegistryNotFound => (StatusCode::NOT_FOUND, error.to_string()),
                RepoError::RegistryInUse => (StatusCode::CONFLICT, error.to_string()),
                RepoError::InvalidCredentials => (StatusCode::UNAUTHORIZED, error.to_string()),
                RepoError::LastSuperAdmin => (StatusCode::CONFLICT, error.to_string()),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Auth(error) => match &error {
                crate::auth::AuthError::InvalidCredentials => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::Forbidden => (StatusCode::FORBIDDEN, error.to_string()),
                crate::auth::AuthError::InvalidToken => {
                    (StatusCode::UNAUTHORIZED, error.to_string())
                }
                crate::auth::AuthError::State(_) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::InvalidSecretRef(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Deploy(error) => match &error {
                DeployError::RegistryNotFound => (StatusCode::NOT_FOUND, error.to_string()),
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                ),
            },
            Self::Log(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
            Self::Metrics(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
            Self::NodeMetrics(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
        };
        (status, message).into_response()
    }
}
