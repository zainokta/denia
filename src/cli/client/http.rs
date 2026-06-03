//! Typed `/v1` client wrapper used by `denia auth` and `denia push`. Request
//! bodies for service/deployment upserts use `serde_json::Value` so the client
//! build does not need to depend on the full backend domain crate. See ADR-030.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone)]
pub struct ClientApi {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug, Error)]
pub enum ClientApiError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api returned {status}: {body}")]
    Api { status: StatusCode, body: String },
}

#[derive(Debug, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String,
}

/// Read-model for `/v1/projects`. Deserializes only the fields the client needs;
/// unknown fields in the `Project` payload are ignored by serde.
#[derive(Debug, Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
}

/// Read-model for `/v1/services`. Subset of the backend `ServiceConfig`.
#[derive(Debug, Deserialize)]
pub struct ServiceView {
    pub id: String,
    pub name: String,
    pub project_id: String,
}

/// Read-model for `/v1/deployments`. Subset of the backend `Deployment`.
#[derive(Debug, Deserialize)]
pub struct DeploymentView {
    pub id: String,
    pub service_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateApiTokenRequest<'a> {
    name: &'a str,
}

impl ClientApi {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LoginResponse, ClientApiError> {
        self.post_json_no_auth("/v1/auth/login", &LoginRequest { username, password })
            .await
    }

    pub async fn create_api_token(
        &self,
        bearer: &str,
        name: &str,
    ) -> Result<CreateApiTokenResponse, ClientApiError> {
        self.post_json("/v1/api-tokens", bearer, &CreateApiTokenRequest { name })
            .await
    }

    pub async fn verify_me(&self, bearer: &str) -> Result<serde_json::Value, ClientApiError> {
        self.get_json("/v1/me", bearer).await
    }

    pub async fn list_projects(&self, bearer: &str) -> Result<Vec<ProjectView>, ClientApiError> {
        self.get_json("/v1/projects", bearer).await
    }

    pub async fn list_services(&self, bearer: &str) -> Result<Vec<ServiceView>, ClientApiError> {
        self.get_json("/v1/services", bearer).await
    }

    pub async fn put_service(
        &self,
        bearer: &str,
        body: &serde_json::Value,
    ) -> Result<ServiceView, ClientApiError> {
        self.post_json("/v1/services", bearer, body).await
    }

    pub async fn create_deployment(
        &self,
        bearer: &str,
        body: &serde_json::Value,
    ) -> Result<DeploymentView, ClientApiError> {
        self.post_json("/v1/deployments", bearer, body).await
    }

    async fn get_json<R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        bearer: &str,
    ) -> Result<R, ClientApiError> {
        let response = self
            .http
            .get(format!("{}{}", self.base_url, path))
            .bearer_auth(bearer)
            .send()
            .await?;
        decode_response(response).await
    }

    async fn post_json_no_auth<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, ClientApiError> {
        let response = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .json(body)
            .send()
            .await?;
        decode_response(response).await
    }

    async fn post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        bearer: &str,
        body: &T,
    ) -> Result<R, ClientApiError> {
        let response = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .bearer_auth(bearer)
            .json(body)
            .send()
            .await?;
        decode_response(response).await
    }
}

async fn decode_response<R: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<R, ClientApiError> {
    let status = response.status();
    if status.is_success() {
        Ok(response.json().await?)
    } else {
        let body = response.text().await.unwrap_or_default();
        Err(ClientApiError::Api { status, body })
    }
}
