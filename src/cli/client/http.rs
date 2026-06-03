//! Minimal HTTP client for the Denia management API used by client subcommands.
//! Talks to a remote `/v1` over bearer auth and derives the websocket URL for
//! the console upgrade. See ADR-033.

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

#[derive(Debug, serde::Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ApiTokenResponse {
    pub id: String,
    pub name: String,
    pub token: String,
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
}
