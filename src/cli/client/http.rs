//! Minimal HTTP client for the Denia management API used by client subcommands.
//! Talks to a remote `/v1` over bearer auth and derives the websocket URL for
//! the console upgrade. See ADR-033.

use futures_util::StreamExt as _;
use reqwest::StatusCode;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum ClientApiError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error {status}: {body}")]
    Api { status: StatusCode, body: String },
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ServiceView {
    pub id: String,
    pub name: String,
    pub project_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
}

/// Response from `GET /v1/node`.
#[derive(Debug, serde::Deserialize)]
pub struct NodeInfo {
    pub control_domain: Option<String>,
}

/// Response from `POST /v1/services/{id}/uploads`.
#[derive(Debug, serde::Deserialize)]
pub struct UploadResponse {
    pub upload_id: String,
}

/// Minimal deployment ref returned by `POST /v1/deployments` (202 Accepted).
/// The server returns the full `Deployment` JSON; we only need the `id`.
#[derive(Debug, serde::Deserialize)]
pub struct DeploymentRef {
    pub id: String,
}

/// Status view returned by `GET /v1/deployments/{id}`.
///
/// `status` is the PascalCase serialisation of `DeploymentStatus`:
/// `"Pending"` | `"Building"` | `"Starting"` | `"Healthy"` | `"Failed"` |
/// `"Stopped"` | `"Inactive"`.  The server enum has no serde rename attribute,
/// so the string form matches the variant name exactly.
#[derive(Debug, serde::Deserialize)]
pub struct DeploymentStatusView {
    pub id: String,
    pub status: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ConsoleReplicaView {
    pub service_id: String,
    pub service_name: String,
    pub deployment_id: String,
    pub replica_index: u32,
    pub state: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ConsoleTicketView {
    pub ticket: String,
    pub expires_at: String,
    pub ws_path: String,
}

#[derive(Debug, Serialize)]
struct ConsoleTicketRequest {
    replica_index: u32,
    cols: u16,
    rows: u16,
}

#[derive(serde::Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

impl std::fmt::Debug for LoginResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginResponse")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(serde::Deserialize)]
pub struct ApiTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String,
}

impl std::fmt::Debug for ApiTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiTokenResponse")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub struct ClientApi {
    base_url: String,
    http: reqwest::Client,
}

impl ClientApi {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        bearer: &str,
    ) -> Result<T, ClientApiError> {
        let response = self
            .http
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(bearer)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientApiError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json::<T>().await?)
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        bearer: &str,
        body: &B,
    ) -> Result<T, ClientApiError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(bearer)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientApiError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json::<T>().await?)
    }

    pub async fn list_services(&self, bearer: &str) -> Result<Vec<ServiceView>, ClientApiError> {
        self.get_json("/v1/services", bearer).await
    }

    pub async fn list_projects(&self, bearer: &str) -> Result<Vec<ProjectView>, ClientApiError> {
        self.get_json("/v1/projects", bearer).await
    }

    pub async fn list_console_replicas(
        &self,
        bearer: &str,
        service_id: &str,
    ) -> Result<Vec<ConsoleReplicaView>, ClientApiError> {
        self.get_json(
            &format!("/v1/services/{service_id}/console/replicas"),
            bearer,
        )
        .await
    }

    pub async fn create_console_ticket(
        &self,
        bearer: &str,
        service_id: &str,
        replica_index: u32,
        cols: u16,
        rows: u16,
    ) -> Result<ConsoleTicketView, ClientApiError> {
        self.post_json(
            &format!("/v1/services/{service_id}/console/tickets"),
            bearer,
            &ConsoleTicketRequest {
                replica_index,
                cols,
                rows,
            },
        )
        .await
    }

    /// POST /v1/auth/login without bearer. Returns the session token.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LoginResponse, ClientApiError> {
        let response = self
            .http
            .post(format!("{}/v1/auth/login", self.base_url))
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientApiError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json::<LoginResponse>().await?)
    }

    /// POST /v1/api-tokens with bearer; returns the freshly-minted long-lived token (shown once).
    pub async fn create_api_token(
        &self,
        bearer: &str,
        name: &str,
    ) -> Result<ApiTokenResponse, ClientApiError> {
        self.post_json("/v1/api-tokens", bearer, &serde_json::json!({ "name": name }))
            .await
    }

    /// GET /v1/me — used to verify a token works. Returns the raw JSON.
    pub async fn me(&self, bearer: &str) -> Result<serde_json::Value, ClientApiError> {
        self.get_json("/v1/me", bearer).await
    }

    /// GET /v1/node — returns node info including the control domain.
    pub async fn node_info(&self, bearer: &str) -> Result<NodeInfo, ClientApiError> {
        self.get_json("/v1/node", bearer).await
    }

    /// POST /v1/projects — create a project with the given name.
    /// Body: `{"name": "<name>"}`.
    pub async fn create_project(
        &self,
        bearer: &str,
        name: &str,
    ) -> Result<ProjectView, ClientApiError> {
        self.post_json("/v1/projects", bearer, &serde_json::json!({ "name": name }))
            .await
    }

    /// POST /v1/services — upsert (create/update) a service.
    /// The caller supplies the full `ServiceConfig`-shaped JSON value.
    /// Returns the persisted service on success (200 OK).
    pub async fn create_service(
        &self,
        bearer: &str,
        body: &serde_json::Value,
    ) -> Result<ServiceView, ClientApiError> {
        self.post_json("/v1/services", bearer, body).await
    }

    /// POST /v1/services/{service_id}/uploads — upload a raw `tar.zst` context archive.
    /// Sends the bytes with `content-type: application/zstd` and returns the
    /// server-assigned `upload_id` (a UUIDv7 string).
    pub async fn upload_context(
        &self,
        bearer: &str,
        service_id: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadResponse, ClientApiError> {
        let response = self
            .http
            .post(format!(
                "{}/v1/services/{service_id}/uploads",
                self.base_url
            ))
            .bearer_auth(bearer)
            .header("content-type", "application/zstd")
            .body(bytes)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientApiError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        Ok(response.json::<UploadResponse>().await?)
    }

    /// POST /v1/deployments — kick off an async deploy.
    /// Returns `202 Accepted` with a `DeploymentRef` (only the `id` is extracted).
    pub async fn create_deployment(
        &self,
        bearer: &str,
        body: &serde_json::Value,
    ) -> Result<DeploymentRef, ClientApiError> {
        self.post_json("/v1/deployments", bearer, body).await
    }

    /// GET /v1/deployments/{deployment_id} — poll deployment status.
    pub async fn get_deployment(
        &self,
        bearer: &str,
        deployment_id: &str,
    ) -> Result<DeploymentStatusView, ClientApiError> {
        self.get_json(&format!("/v1/deployments/{deployment_id}"), bearer)
            .await
    }

    /// GET /v1/deployments/{deployment_id}/logs — SSE deployment log tail.
    /// Streams each chunk/line to stdout as it arrives. Returns `Ok(())` when
    /// the stream ends (server sends `done` event or connection closes).
    pub async fn stream_deployment_logs(
        &self,
        bearer: &str,
        deployment_id: &str,
    ) -> Result<(), ClientApiError> {
        let response = self
            .http
            .get(format!(
                "{}/v1/deployments/{deployment_id}/logs",
                self.base_url
            ))
            .bearer_auth(bearer)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientApiError::Api {
                status,
                body: response.text().await.unwrap_or_default(),
            });
        }
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Ok(text) = std::str::from_utf8(&chunk) {
                print!("{text}");
            }
        }
        Ok(())
    }

    /// Turn a `ws_path` (e.g. `/v1/services/{id}/console/ws?ticket=...`) into an
    /// absolute `ws://`/`wss://` URL against the profile's base URL.
    pub fn websocket_url(&self, ws_path: &str) -> Result<String, ClientApiError> {
        let mut url = reqwest::Url::parse(&self.base_url).map_err(|error| ClientApiError::Api {
            status: StatusCode::BAD_REQUEST,
            body: error.to_string(),
        })?;
        match url.scheme() {
            "http" => url
                .set_scheme("ws")
                .map_err(|_| scheme_error("could not set websocket scheme"))?,
            "https" => url
                .set_scheme("wss")
                .map_err(|_| scheme_error("could not set websocket scheme"))?,
            _ => {
                return Err(scheme_error("profile url must use http or https"));
            }
        }
        url.set_path(ws_path.split('?').next().unwrap_or(ws_path));
        url.set_query(ws_path.split_once('?').map(|(_, query)| query));
        Ok(url.to_string())
    }
}

fn scheme_error(message: &str) -> ClientApiError {
    ClientApiError::Api {
        status: StatusCode::BAD_REQUEST,
        body: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_returns_token() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/auth/login");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"token":"sess","expires_at":"2026-01-01T00:00:00Z"}"#);
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let resp = api.login("u", "p").await.unwrap();
        assert_eq!(resp.token, "sess");
    }

    #[tokio::test]
    async fn create_api_token_returns_token() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/api-tokens")
                    .header("Authorization", "Bearer mybearer")
                    .json_body(serde_json::json!({ "name": "denia-cli" }));
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"id":"abc","name":"denia-cli","token":"tok"}"#);
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let resp = api.create_api_token("mybearer", "denia-cli").await.unwrap();
        assert_eq!(resp.token, "tok");
        assert_eq!(resp.name, "denia-cli");
    }

    #[tokio::test]
    async fn me_succeeds_with_token() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/me");
                then.status(200)
                    .header("content-type", "application/json")
                    .body("{}");
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        assert!(api.me("tok").await.is_ok());
    }

    #[test]
    fn websocket_url_swaps_http_to_ws() {
        let api = ClientApi::new("http://denia.example.com");
        let url = api
            .websocket_url("/v1/services/abc/console/ws?ticket=xyz")
            .unwrap();
        assert_eq!(
            url,
            "ws://denia.example.com/v1/services/abc/console/ws?ticket=xyz"
        );
    }

    #[test]
    fn websocket_url_swaps_https_to_wss() {
        let api = ClientApi::new("https://denia.example.com");
        let url = api
            .websocket_url("/v1/services/abc/console/ws?ticket=xyz")
            .unwrap();
        assert!(url.starts_with("wss://denia.example.com/v1/services/abc/console/ws?ticket=xyz"));
    }

    #[tokio::test]
    async fn node_info_parses_control_domain() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/node");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"control_domain":"d.example.com"}"#);
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let info = api.node_info("tok").await.unwrap();
        assert_eq!(info.control_domain.as_deref(), Some("d.example.com"));
    }

    #[tokio::test]
    async fn create_project_returns_view() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/v1/projects")
                    .json_body(serde_json::json!({ "name": "my-project" }));
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"id":"01900000-0000-7000-8000-000000000001","name":"my-project"}"#);
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let view = api.create_project("tok", "my-project").await.unwrap();
        assert_eq!(view.name, "my-project");
        assert_eq!(view.id, "01900000-0000-7000-8000-000000000001");
    }

    #[tokio::test]
    async fn upload_context_sends_zstd_body() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let svc_id = "01900000-0000-7000-8000-000000000002";
        let _m = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(format!("/v1/services/{svc_id}/uploads"))
                    .header("content-type", "application/zstd");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"upload_id":"u1","expires_at":"2026-01-01T00:00:00Z"}"#);
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let resp = api
            .upload_context("tok", svc_id, b"fake-zstd".to_vec())
            .await
            .unwrap();
        assert_eq!(resp.upload_id, "u1");
    }

    #[tokio::test]
    async fn create_deployment_accepts_202() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/deployments");
                then.status(202)
                    .header("content-type", "application/json")
                    .body(
                        r#"{"id":"d1","service_id":"s1","request":{"source":"external_image","service_id":"s1","image":"nginx"},"status":"Pending","created_at":"2026-01-01T00:00:00Z"}"#,
                    );
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let deploy_ref = api
            .create_deployment("tok", &serde_json::json!({"source":"external_image","service_id":"s1","image":"nginx"}))
            .await
            .unwrap();
        assert_eq!(deploy_ref.id, "d1");
    }

    #[tokio::test]
    async fn get_deployment_returns_status() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/deployments/d1");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{"id":"d1","service_id":"s1","request":{"source":"external_image","service_id":"s1","image":"nginx"},"status":"Healthy","created_at":"2026-01-01T00:00:00Z"}"#,
                    );
            })
            .await;
        let api = ClientApi::new(&server.base_url());
        let view = api.get_deployment("tok", "d1").await.unwrap();
        assert_eq!(view.id, "d1");
        assert_eq!(view.status, "Healthy");
    }
}
