use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cornu_core::{
    Auth, ClientAuth, ClientOptions, Grant, HttpClient, Method, OAuth2, Request, SendError, Token,
};
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> HttpClient {
    HttpClient::new(&ClientOptions::default()).unwrap()
}

fn config(server: &MockServer) -> OAuth2 {
    OAuth2 {
        token_url: format!("{}/token", server.uri()),
        client_id: "app".into(),
        client_secret: "s3cret".into(),
        scope: "read".into(),
        ..Default::default()
    }
}

async fn api(server: &MockServer, token: &str) {
    Mock::given(path("/api"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

fn oauth_error(result: Result<impl std::fmt::Debug, SendError>) -> String {
    match result {
        Err(SendError::OAuth(message)) => message,
        other => panic!("expected an OAuth error, got {other:?}"),
    }
}

#[tokio::test]
async fn client_credentials_token_is_fetched_once_and_injected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(header("authorization", "Basic YXBwOnMzY3JldA=="))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("scope=read"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access_token": "t1", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    api(&server, "t1").await;

    let client = client();
    let request = Request::new(Method::Get, format!("{}/api", server.uri()))
        .with_auth(Auth::OAuth2(config(&server)));
    for _ in 0..2 {
        assert_eq!(client.send(&request).await.unwrap().status, 200);
    }
    let token = client.tokens().get(&config(&server)).unwrap();
    assert_eq!(token.access_token, "t1");
    assert!(token.expires_at.is_some());
}

#[tokio::test]
async fn secret_can_go_in_the_body() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .and(body_string_contains("client_id=app&client_secret=s3cret"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("access_token=t1&token_type=bearer"),
        )
        .mount(&server)
        .await;
    let mut config = config(&server);
    config.client_auth = ClientAuth::Body;
    let token = client().new_token(&config, |_| {}).await.unwrap();
    assert_eq!(
        token.access_token, "t1",
        "form-encoded answers are understood"
    );
}

#[tokio::test]
async fn expired_token_is_refreshed() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .and(body_string_contains(
            "grant_type=refresh_token&refresh_token=r1",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access_token": "t2", "expires_in": 60})),
        )
        .expect(1)
        .mount(&server)
        .await;
    api(&server, "t2").await;

    let client = client();
    let mut config = config(&server);
    config.grant = Grant::AuthorizationCode;
    client.tokens().set(
        &config,
        Token {
            access_token: "t1".into(),
            refresh_token: Some("r1".into()),
            expires_at: Some(1),
        },
    );
    let request = Request::new(Method::Get, format!("{}/api", server.uri()))
        .with_auth(Auth::OAuth2(config.clone()));
    assert_eq!(client.send(&request).await.unwrap().status, 200);
    assert_eq!(
        client
            .tokens()
            .get(&config)
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("r1"),
        "the refresh token is kept when the server sends no new one"
    );
}

#[tokio::test]
async fn server_errors_and_missing_tokens_are_reported() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            serde_json::json!({"error": "invalid_client", "error_description": "bad secret"}),
        ))
        .mount(&server)
        .await;
    let client = client();
    let request = Request::new(Method::Get, format!("{}/api", server.uri()))
        .with_auth(Auth::OAuth2(config(&server)));
    assert_eq!(
        oauth_error(client.send(&request).await),
        "invalid_client: bad secret"
    );

    let mut code = config(&server);
    code.grant = Grant::AuthorizationCode;
    let request = request.with_auth(Auth::OAuth2(code));
    assert!(oauth_error(client.send(&request).await).contains("authorize"));
}

/// Plays the browser: follows the authorization URL straight to the redirect URI.
fn browser(
    redirect_query: impl Fn(&HashMap<String, String>) -> String + Send + 'static,
) -> impl FnOnce(&url::Url) + Send {
    move |url| {
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let target = format!("{}?{}", params["redirect_uri"], redirect_query(&params));
        tokio::spawn(async move {
            let page = reqwest::get(target).await.unwrap().text().await.unwrap();
            assert!(page.contains("cornu"));
        });
    }
}

#[tokio::test]
async fn authorization_code_with_pkce_goes_through_the_loopback_redirect() {
    let server = MockServer::start().await;
    Mock::given(path("/token"))
        .and(body_string_contains(
            "grant_type=authorization_code&code=c1",
        ))
        .and(body_string_contains("client_id=app"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"access_token": "t1", "refresh_token": "r1", "expires_in": 60}),
        ))
        .mount(&server)
        .await;
    let config = OAuth2 {
        grant: Grant::AuthorizationCode,
        auth_url: format!("{}/authorize?audience=api", server.uri()),
        client_secret: String::new(),
        redirect_uri: "http://127.0.0.1:0/callback".into(),
        ..config(&server)
    };

    let challenge = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen = challenge.clone();
    let token = client()
        .new_token(
            &config,
            browser(move |params| {
                assert_eq!(params["response_type"], "code");
                assert_eq!(params["code_challenge_method"], "S256");
                assert_eq!(params["audience"], "api");
                assert_eq!(params["scope"], "read");
                assert!(!params["redirect_uri"].ends_with(":0/callback"));
                *seen.lock().unwrap() = params["code_challenge"].clone();
                format!("code=c1&state={}", params["state"])
            }),
        )
        .await
        .unwrap();
    assert_eq!(token.access_token, "t1");
    assert_eq!(token.refresh_token.as_deref(), Some("r1"));

    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body).into_owned();
    let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
    assert_eq!(digest, *challenge.lock().unwrap());
    assert!(requests[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn authorization_code_rejects_a_foreign_state_and_denials() {
    let server = MockServer::start().await;
    let config = OAuth2 {
        grant: Grant::AuthorizationCode,
        auth_url: format!("{}/authorize", server.uri()),
        redirect_uri: "http://127.0.0.1:0/callback".into(),
        ..config(&server)
    };
    let client = client();
    let forged = client
        .new_token(&config, browser(|_| "code=c1&state=forged".into()))
        .await;
    assert!(oauth_error(forged).contains("state"));

    let denied = client
        .new_token(
            &config,
            browser(|params| {
                format!(
                    "error=access_denied&error_description=User%20said%20no&state={}",
                    params["state"]
                )
            }),
        )
        .await;
    assert_eq!(oauth_error(denied), "access_denied: User said no");
    assert!(client.tokens().is_empty());
}
