use cornu_core::runner::{CancelToken, RunPlan, Step, run};
use cornu_core::{
    Assertion, ClientOptions, HttpClient, Method, NamedScripts, Request, Scripts, SendError,
    Variables, exchange,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn named(origin: &str, pre: &str, post: &str) -> NamedScripts {
    NamedScripts {
        origin: origin.into(),
        scripts: Scripts {
            pre: pre.into(),
            post: post.into(),
        },
    }
}

fn client() -> HttpClient {
    HttpClient::new(&ClientOptions::default()).unwrap()
}

async fn send(
    request: &Request,
    variables: &Variables,
    scripts: &[NamedScripts],
) -> cornu_core::Exchange {
    exchange(
        &client(),
        request,
        variables,
        scripts,
        std::future::pending(),
        |_| {},
    )
    .await
}

#[tokio::test]
async fn folder_then_request_scripts_sign_send_and_check() {
    let server = MockServer::start().await;
    let url = format!("{}/orders?tenant=acme", server.uri());
    let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
    mac.update(format!("POST{url}").as_bytes());
    let signature: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("x-signature", signature.as_str()))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": 42, "items": [1, 2]})),
        )
        .mount(&server)
        .await;

    let request = Request::new(Method::Post, "{{base}}/orders")
        .with_query("tenant", "{{tenant}}")
        .with_assertion(Assertion::status(201));
    let variables =
        Variables::new().with_layer([("base", server.uri()), ("secret", "s3cret".into())]);
    let scripts = [
        named(
            "folder Api",
            "vars.tenant = \"acme\";",
            "print(`status ${response.status}`);",
        ),
        named(
            "the request",
            r#"
                let url = resolve(request.url) + "?tenant=" + vars.tenant;
                request.headers["X-Signature"] = hmac_sha256(vars.secret, request.method + url);
            "#,
            r#"
                test("two items", response.json.items.len() == 2);
                vars.order_id = response.json.id;
            "#,
        ),
    ];
    let exchange = send(&request, &variables, &scripts).await;
    assert!(
        exchange.passed(),
        "{:?} {:?}",
        exchange.outcome,
        exchange.assertions
    );
    assert_eq!(exchange.request.url, format!("{}/orders", server.uri()));
    let set: Vec<_> = exchange
        .captured
        .iter()
        .map(|captured| (captured.variable.as_str(), captured.value.clone().unwrap()))
        .collect();
    assert_eq!(set, [("tenant", "acme".into()), ("order_id", "42".into())]);
    let tests: Vec<_> = exchange
        .assertions
        .iter()
        .map(|result| (result.description.as_str(), result.passed()))
        .collect();
    assert_eq!(tests, [("status equals 201", true), ("two items", true)]);
    assert_eq!(exchange.logs, ["status 201"]);
}

#[tokio::test]
async fn a_failing_pre_request_script_sends_nothing() {
    let server = MockServer::start().await;
    let request = Request::new(Method::Get, server.uri());
    let exchange = send(
        &request,
        &Variables::new(),
        &[named(
            "the request",
            "print(\"before\");\nthrow \"no key\";",
            "",
        )],
    )
    .await;
    let Err(SendError::Script(error)) = &exchange.outcome else {
        panic!("{:?}", exchange.outcome);
    };
    assert!(
        error.contains("pre-request script of the request") && error.contains("no key"),
        "{error}"
    );
    assert_eq!(exchange.logs, ["before"]);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_post_response_script_fails_the_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let request = Request::new(Method::Get, server.uri());
    let exchange = send(
        &request,
        &Variables::new(),
        &[named("the request", "", "response.json.missing.field")],
    )
    .await;
    assert!(!exchange.passed());
    let failure = exchange.assertions.last().unwrap();
    assert_eq!(failure.description, "post-response script");
    assert!(
        failure
            .error
            .as_deref()
            .unwrap()
            .contains("post-response script of the request")
    );
}

#[tokio::test]
async fn variables_set_by_scripts_feed_the_next_steps_of_a_run() {
    let server = MockServer::start().await;
    Mock::given(path("/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"user": {"id": 7}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/users/7"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let step = |url: &str, post: &str| Step {
        request: Request::new(Method::Get, format!("{}{url}", server.uri())),
        variables: Variables::new(),
        scripts: vec![named("the request", "", post)],
    };
    let plan = RunPlan {
        steps: vec![
            step("/login", "vars.user_id = response.json.user.id;"),
            step(
                "/users/{{user_id}}",
                "test(\"found\", response.status == 200);",
            ),
        ],
        rows: Vec::new(),
        options: Default::default(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let summary = run(&client(), plan, &CancelToken::default(), tx).await;
    assert_eq!(summary.succeeded, 2, "{summary:?}");
    let mut results = Vec::new();
    while let Ok(result) = rx.try_recv() {
        results.push(result);
    }
    results.sort_by_key(|result| result.step);
    assert_eq!(results[1].request.url, format!("{}/users/7", server.uri()));
    assert_eq!(results[1].assertions[0].description, "found");
}
