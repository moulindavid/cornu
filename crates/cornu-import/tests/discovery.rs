use cornu_core::{ClientOptions, HttpClient};
use cornu_import::discovery::{DiscoveryError, discover};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SPEC: &str = r#"{"openapi": "3.0.0", "info": {"title": "Found"}, "servers": [{"url": "/api"}],
    "paths": {"/ping": {"get": {}}}}"#;

fn client() -> HttpClient {
    HttpClient::new(&ClientOptions::default()).unwrap()
}

async fn serve(route: &str, template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn direct_spec_url_is_used_as_is() {
    let server = serve("/spec", ResponseTemplate::new(200).set_body_string(SPEC)).await;
    let url = format!("{}/spec", server.uri());
    let found = discover(&client(), &url).await.unwrap();
    assert_eq!(found.url, url);
    assert_eq!(found.collection.root.name, "Found");
    assert_eq!(
        found.collection.root.variables[0].value,
        format!("{}/api", server.uri())
    );
}

#[tokio::test]
async fn well_known_locations_are_probed_from_the_base_url() {
    let server = serve(
        "/service/v3/api-docs",
        ResponseTemplate::new(200).set_body_string(SPEC),
    )
    .await;
    let found = discover(&client(), &format!("{}/service/", server.uri()))
        .await
        .unwrap();
    assert_eq!(found.url, format!("{}/service/v3/api-docs", server.uri()));
}

#[tokio::test]
async fn docs_pages_lead_to_their_spec() {
    let server = serve(
        "/docs",
        ResponseTemplate::new(200)
            .set_body_string(r#"<script>SwaggerUIBundle({url: "/custom/spec.json"})</script>"#),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/custom/spec.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPEC))
        .mount(&server)
        .await;
    let found = discover(&client(), &format!("{}/docs", server.uri()))
        .await
        .unwrap();
    assert_eq!(found.url, format!("{}/custom/spec.json", server.uri()));
}

#[tokio::test]
async fn swagger_ui_initializer_scripts_lead_to_their_spec() {
    let server = serve(
        "/",
        ResponseTemplate::new(200)
            .set_body_string(r#"<script src="./swagger-initializer.js"></script>"#),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/swagger-initializer.js"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"window.ui = SwaggerUIBundle({ url: "/api/v3/openapi.json" })"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SPEC))
        .mount(&server)
        .await;
    let found = discover(&client(), &server.uri()).await.unwrap();
    assert_eq!(found.url, format!("{}/api/v3/openapi.json", server.uri()));
}

#[tokio::test]
async fn missing_specs_and_dead_hosts_are_reported() {
    let server = MockServer::start().await;
    let error = discover(&client(), &server.uri()).await.unwrap_err();
    assert!(matches!(error, DiscoveryError::NotFound { tried } if tried > 10));

    let dead = "http://127.0.0.1:9";
    assert!(matches!(
        discover(&client(), dead).await,
        Err(DiscoveryError::Unreachable(_))
    ));
    assert!(matches!(
        discover(&client(), "http://exa mple").await,
        Err(DiscoveryError::InvalidUrl(_))
    ));
}
