use denia::cli::client::http::{ClientApi, CreateApiTokenResponse};
use httpmock::prelude::*;

#[tokio::test]
async fn login_and_create_token() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(POST).path("/v1/auth/login");
            then.status(200).json_body_obj(&serde_json::json!({
                "token": "session-token",
                "expires_at": "2026-06-03T00:00:00Z"
            }));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/v1/api-tokens")
                .header("Authorization", "Bearer session-token");
            then.status(201).json_body_obj(&serde_json::json!({
                "id": "01976f2e-0000-7000-8000-000000000000",
                "name": "denia-cli-test",
                "token": "api-token"
            }));
        })
        .await;

    let api = ClientApi::new(server.url(""));
    let session = api.login("zain", "secret").await.unwrap();
    let token: CreateApiTokenResponse = api
        .create_api_token(&session.token, "denia-cli-test")
        .await
        .unwrap();
    assert_eq!(token.token, "api-token");
}
