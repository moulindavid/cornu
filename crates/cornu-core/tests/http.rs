use std::time::Duration;

use cornu_core::{
    ApiKeyLocation, Auth, Body, ClientOptions, HttpClient, KeyValue, Method, MultipartPart,
    Request, SendError, Variables,
};
use wiremock::matchers::{body_string, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> HttpClient {
    HttpClient::new(&ClientOptions::default()).unwrap()
}

#[tokio::test]
async fn sends_enabled_query_and_headers_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/users"))
        .and(query_param("page", "2"))
        .and(header("x-trace", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let mut request = Request::new(Method::Get, format!("{}/users", server.uri()))
        .with_query("page", "2")
        .with_header("X-Trace", "abc");
    request.query.push(KeyValue::new("debug", "1").disabled());
    request
        .headers
        .push(KeyValue::new("X-Secret", "1").disabled());

    let response = client().send(&request).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.text(), "ok");
    assert_eq!(response.size(), 2);

    let received = &server.received_requests().await.unwrap()[0];
    assert_eq!(received.url.query(), Some("page=2"));
    assert!(!received.headers.contains_key("x-secret"));
}

#[tokio::test]
async fn json_body_sets_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("content-type", "application/json"))
        .and(body_string(r#"{"name":"cornu"}"#))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, server.uri()).with_body(Body::Json {
        text: r#"{"name":"cornu"}"#.into(),
    });
    assert_eq!(client().send(&request).await.unwrap().status, 201);
}

#[tokio::test]
async fn explicit_content_type_header_wins() {
    let server = MockServer::start().await;
    Mock::given(header("content-type", "application/vnd.api+json"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, server.uri())
        .with_header("Content-Type", "application/vnd.api+json")
        .with_body(Body::Json { text: "{}".into() });
    client().send(&request).await.unwrap();
}

#[tokio::test]
async fn form_body_is_url_encoded() {
    let server = MockServer::start().await;
    Mock::given(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string("q=a+b&lang=fr"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, server.uri()).with_body(Body::FormUrlEncoded {
        fields: vec![
            KeyValue::new("q", "a b"),
            KeyValue::new("skip", "x").disabled(),
            KeyValue::new("lang", "fr"),
        ],
    });
    client().send(&request).await.unwrap();
}

#[tokio::test]
async fn multipart_sends_text_and_file_parts() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("hello.txt");
    std::fs::write(&file, "file content").unwrap();

    let server = MockServer::start().await;
    Mock::given(body_string_contains("name=\"title\""))
        .and(body_string_contains("filename=\"hello.txt\""))
        .and(body_string_contains("file content"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, server.uri()).with_body(Body::Multipart {
        parts: vec![
            MultipartPart::text("title", "cornu"),
            MultipartPart::file("upload", &file),
        ],
    });
    client().send(&request).await.unwrap();

    let received = &server.received_requests().await.unwrap()[0];
    let content_type = received.headers["content-type"].to_str().unwrap();
    assert!(content_type.starts_with("multipart/form-data; boundary="));
}

#[tokio::test]
async fn binary_body_sends_file_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.bin");
    std::fs::write(&file, [0u8, 1, 2, 255]).unwrap();

    let server = MockServer::start().await;
    Mock::given(header("content-type", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Put, server.uri()).with_body(Body::Binary {
        path: file,
        content_type: None,
    });
    client().send(&request).await.unwrap();
    assert_eq!(
        server.received_requests().await.unwrap()[0].body,
        [0, 1, 2, 255]
    );
}

#[tokio::test]
async fn missing_file_is_reported() {
    let request = Request::new(Method::Put, "http://127.0.0.1:1").with_body(Body::Binary {
        path: "/definitely/missing.bin".into(),
        content_type: None,
    });
    let error = client().send(&request).await.unwrap_err();
    assert!(matches!(error, SendError::ReadFile { .. }), "{error}");
}

#[tokio::test]
async fn error_statuses_are_responses_not_errors() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(
            ResponseTemplate::new(404)
                .insert_header("x-request-id", "42")
                .set_body_string("not found"),
        )
        .mount(&server)
        .await;

    let response = client()
        .send(&Request::new(Method::Delete, server.uri()))
        .await
        .unwrap();
    assert_eq!(response.status, 404);
    assert_eq!(response.header("X-Request-Id"), Some("42"));
}

#[tokio::test]
async fn custom_methods_are_sent_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("PURGE"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Custom("PURGE".into()), server.uri());
    client().send(&request).await.unwrap();
}

#[tokio::test]
async fn redirects_follow_option() {
    let server = MockServer::start().await;
    Mock::given(path("/old"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/new"))
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("moved"))
        .mount(&server)
        .await;
    let request = Request::new(Method::Get, format!("{}/old", server.uri()));

    let followed = client().send(&request).await.unwrap();
    assert_eq!((followed.status, followed.text().as_ref()), (200, "moved"));

    let options = ClientOptions {
        follow_redirects: false,
        ..Default::default()
    };
    let not_followed = HttpClient::new(&options)
        .unwrap()
        .send(&request)
        .await
        .unwrap();
    assert_eq!(not_followed.status, 302);
    assert_eq!(not_followed.header("location"), Some("/new"));
}

#[tokio::test]
async fn timeout_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let options = ClientOptions {
        timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let error = HttpClient::new(&options)
        .unwrap()
        .send(&Request::new(Method::Get, server.uri()))
        .await
        .unwrap_err();
    assert!(matches!(error, SendError::Timeout), "{error}");
}

#[tokio::test]
async fn elapsed_covers_the_full_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(50)))
        .mount(&server)
        .await;

    let response = client()
        .send(&Request::new(Method::Get, server.uri()))
        .await
        .unwrap();
    assert!(response.elapsed >= Duration::from_millis(50));
}

#[tokio::test]
async fn url_without_scheme_defaults_to_http() {
    let server = MockServer::start().await;
    Mock::given(path("/ping"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let address = server.address();
    let request = Request::new(Method::Get, format!("{address}/ping"));
    assert_eq!(client().send(&request).await.unwrap().status, 204);
}

#[tokio::test]
async fn invalid_inputs_are_rejected_before_sending() {
    let client = client();

    let bad_url = Request::new(Method::Get, "http://exa mple.com");
    assert!(matches!(
        client.send(&bad_url).await,
        Err(SendError::InvalidUrl { .. })
    ));

    let bad_header = Request::new(Method::Get, "http://127.0.0.1:1").with_header("Bad Name", "x");
    assert!(matches!(
        client.send(&bad_header).await,
        Err(SendError::InvalidHeaderName(_))
    ));
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    let request = Request::new(Method::Get, "http://127.0.0.1:1");
    let error = client().send(&request).await.unwrap_err();
    assert!(matches!(error, SendError::Connect(_)), "{error}");
}

#[tokio::test]
async fn bearer_auth_is_injected_after_variable_resolution() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Bearer s3cret"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let folder_auth = Auth::Bearer {
        token: "{{token}}".into(),
    };
    let mut request = Request::new(Method::Get, server.uri());
    request.auth = Auth::effective([&request.auth, &folder_auth]);
    let variables = Variables::new().with_layer([("token", "s3cret")]);

    let resolved = request.resolve(&variables);
    assert!(resolved.unresolved.is_empty());
    client().send(&resolved.request).await.unwrap();
}

#[tokio::test]
async fn explicit_authorization_header_wins_over_auth() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Custom xyz"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Get, server.uri())
        .with_header("Authorization", "Custom xyz")
        .with_auth(Auth::Bearer {
            token: "ignored".into(),
        });
    client().send(&request).await.unwrap();
    let received = &server.received_requests().await.unwrap()[0];
    assert_eq!(received.headers.get_all("authorization").iter().count(), 1);
}

#[tokio::test]
async fn basic_auth_header_is_sent() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Get, server.uri()).with_auth(Auth::Basic {
        username: "user".into(),
        password: "pass".into(),
    });
    client().send(&request).await.unwrap();
}

#[tokio::test]
async fn api_key_in_query_is_appended() {
    let server = MockServer::start().await;
    Mock::given(query_param("api_key", "k1"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Get, server.uri())
        .with_query("page", "1")
        .with_auth(Auth::ApiKey {
            key: "api_key".into(),
            value: "k1".into(),
            location: ApiKeyLocation::Query,
        });
    client().send(&request).await.unwrap();
}

#[tokio::test]
async fn graphql_body_is_sent_as_json_with_resolved_variables() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("content-type", "application/json"))
        .and(body_string(
            r#"{"query":"query ($code: ID!) { country(code: $code) { name } }","variables":{"code":"FR","limit":3}}"#,
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, server.uri()).with_body(Body::GraphQl {
        query: "query ($code: ID!) { country(code: $code) { name } }".into(),
        variables: r#"{"code": "{{code}}", "limit": {{limit}}}"#.into(),
    });
    let variables = Variables::new().with_layer([("code", "FR"), ("limit", "3")]);
    let resolved = request.resolve(&variables);
    assert!(resolved.unresolved.is_empty());
    assert_eq!(client().send(&resolved.request).await.unwrap().status, 200);

    let broken = Request::new(Method::Post, server.uri()).with_body(Body::GraphQl {
        query: "{ a }".into(),
        variables: "{ nope".into(),
    });
    assert!(matches!(
        client().send(&broken).await,
        Err(SendError::InvalidGraphqlVariables(_))
    ));
}
