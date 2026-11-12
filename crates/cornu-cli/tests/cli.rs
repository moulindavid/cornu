use std::path::Path;
use std::process::Command;

use cornu_core::{Assertion, Capture, Environment, KeyValue, Method, Operator, Request};
use cornu_storage::{EnvironmentEntry, RequestEntry, Workspace};
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
    server: MockServer,
}

/// A workspace with an `api` folder: `1-login` captures a token that `2-profile` sends, and
/// `3-hello/{{name}}` greets. The `dev` environment points at a mock server.
fn fixture() -> Fixture {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"token": "t-123"})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/profile"))
            .and(header("authorization", "Bearer t-123"))
            .and(header("x-api-key", "very-secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 7})))
            .mount(&server)
            .await;
        Mock::given(path("/hello/ada"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hi ada"))
            .mount(&server)
            .await;
        Mock::given(path("/hello/bob"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hi bob"))
            .mount(&server)
            .await;
        server
    });

    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(dir.path(), "Demo").unwrap();
    std::fs::create_dir_all(dir.path().join("api")).unwrap();
    let requests = [
        (
            "api/1-login.req.toml",
            Request::new(Method::Post, "{{base}}/login")
                .with_name("Login")
                .with_capture(Capture::body("token", "$.token")),
        ),
        (
            "api/2-profile.req.toml",
            Request::new(Method::Get, "{{base}}/profile")
                .with_name("Profile")
                .with_header("Authorization", "Bearer {{token}}")
                .with_header("X-Api-Key", "{{api_key}}")
                .with_assertion(Assertion::status(200))
                .with_assertion(Assertion::body("$.id", Operator::Equals, "7")),
        ),
        (
            "api/3-hello.req.toml",
            Request::new(Method::Get, "{{base}}/hello/{{name}}").with_name("Hello"),
        ),
    ];
    for (path, request) in requests {
        workspace
            .save_request(&RequestEntry {
                path: path.into(),
                request,
            })
            .unwrap();
    }
    let mut environment = Environment::new("dev")
        .with_variable("base", server.uri())
        .with_variable("name", "ada");
    environment
        .variables
        .push(KeyValue::new("api_key", "very-secret-key").secret());
    workspace
        .save_environment(&EnvironmentEntry {
            path: "environments/dev.toml".into(),
            environment,
        })
        .unwrap();
    Fixture {
        dir,
        _runtime: runtime,
        server,
    }
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Output {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!("{error} in {}", self.stdout);
        })
    }
}

fn cli(dir: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cornu-cli"))
        .args(args)
        .current_dir(dir)
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    Output {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn run_chains_captures_and_reports_each_request() {
    let fixture = fixture();
    let output = cli(fixture.dir.path(), &["run", "--env", "dev"]);
    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    for expected in [
        "✔ 200  POST    api/1-login",
        "✔ 200  GET     api/2-profile",
        "✔ 200  GET     api/3-hello",
        "3 requests · 3 passed · 0 failed · 0 errors",
    ] {
        assert!(
            output.stdout.contains(expected),
            "{expected} in {}",
            output.stdout
        );
    }
    assert!(!output.stdout.contains('\u{1b}'), "no colors when piped");

    let failing = cli(
        fixture.dir.path(),
        &["run", "api/2-profile", "--env", "dev"],
    );
    assert_eq!(failing.code, 1);
    assert!(
        failing.stdout.contains("✘ 404  GET     api/2-profile"),
        "{}",
        failing.stdout
    );
    assert!(failing.stdout.contains("✘ status"), "{}", failing.stdout);
    assert!(
        failing.stdout.contains("unresolved variables: token"),
        "{}",
        failing.stdout
    );
}

#[test]
fn json_output_hides_secrets_unless_asked() {
    let fixture = fixture();
    let output = cli(
        fixture.dir.path(),
        &["run", "api", "--env", "dev", "--json"],
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    let report = output.json();
    let profile = &report["results"][1];
    assert_eq!(profile["name"], "api/2-profile");
    assert_eq!(profile["passed"], true);
    assert_eq!(
        profile["request"]["headers"][1],
        serde_json::json!(["X-Api-Key", "•••"])
    );
    assert_eq!(profile["response"]["status"], 200);
    assert_eq!(profile["response"]["body"], "{\"id\":7}");
    assert_eq!(profile["assertions"][1]["passed"], true);
    assert_eq!(report["results"][0]["captures"][0]["value"], "t-123");
    assert_eq!(report["summary"]["passed"], 3);
    assert!(!output.stdout.contains("very-secret-key"));

    let shown = cli(
        fixture.dir.path(),
        &[
            "run",
            "api/2-profile",
            "--env",
            "dev",
            "--var",
            "token=t-123",
            "--json",
            "--show-secrets",
        ],
    );
    assert_eq!(shown.code, 0, "{}", shown.stdout);
    assert!(shown.stdout.contains("very-secret-key"));
}

#[test]
fn data_rows_iterate_and_bail_stops_early() {
    let fixture = fixture();
    let csv = fixture.dir.path().join("names.csv");
    std::fs::write(&csv, "name\nada\nbob\n").unwrap();
    let csv = csv.to_str().unwrap();
    let output = cli(
        fixture.dir.path(),
        &["run", "api/3-hello", "--env", "dev", "--data", csv],
    );
    assert_eq!(output.code, 0, "{}", output.stdout);
    assert!(
        output.stdout.contains("api/3-hello #1"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("api/3-hello #2"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("2 requests · 2 passed"),
        "{}",
        output.stdout
    );

    std::fs::write(fixture.dir.path().join("names.csv"), "zoe\nada\nbob\n").unwrap();
    let output = cli(
        fixture.dir.path(),
        &[
            "run",
            "api/3-hello",
            "--env",
            "dev",
            "--data",
            csv,
            "--no-header",
            "--var",
            "name={{col1}}",
            "--bail",
            "--json",
        ],
    );
    assert_eq!(output.code, 1, "{}", output.stdout);
    let summary = &output.json()["summary"];
    assert_eq!(summary["failed"], 1);
    assert_eq!(summary["skipped"], 2);
}

#[test]
fn send_prints_the_full_response() {
    let fixture = fixture();
    let output = cli(
        fixture.dir.path(),
        &[
            "send",
            "api/2-profile",
            "--env",
            "dev",
            "--var",
            "token=t-123",
        ],
    );
    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    assert!(
        output.stdout.contains("content-type: application/json"),
        "{}",
        output.stdout
    );
    assert!(
        output.stdout.contains("{\n  \"id\": 7\n}"),
        "{}",
        output.stdout
    );

    let json = cli(
        fixture.dir.path(),
        &["send", "api/3-hello", "--env", "dev", "--json"],
    )
    .json();
    assert_eq!(json["response"]["body"], "hi ada");

    let folder = cli(fixture.dir.path(), &["send", "api"]);
    assert_eq!(folder.code, 2);
    assert!(folder.stderr.contains("is a folder"), "{}", folder.stderr);
}

#[test]
fn list_and_envs_describe_the_workspace() {
    let fixture = fixture();
    let list = cli(fixture.dir.path(), &["list", "--json"]).json();
    let paths: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["api/1-login", "api/2-profile", "api/3-hello"]);
    assert_eq!(list[0]["method"], "POST");
    assert_eq!(list[0]["kind"], "http");

    let text = cli(fixture.dir.path(), &["list"]).stdout;
    assert!(
        text.contains("POST    api/1-login") && text.contains("{{base}}/login"),
        "{text}"
    );

    let envs = cli(fixture.dir.path(), &["envs", "--json"]).json();
    let variables = &envs[0]["variables"];
    assert_eq!(envs[0]["name"], "dev");
    assert_eq!(variables[2]["value"], Value::Null);
    assert_eq!(variables[2]["secret"], true);
    let text = cli(fixture.dir.path(), &["envs"]).stdout;
    assert!(
        text.contains("api_key = •••") && !text.contains("very-secret-key"),
        "{text}"
    );
}

#[test]
fn junit_reports_failures_for_ci() {
    let fixture = fixture();
    let report = fixture.dir.path().join("report.xml");
    let output = cli(
        fixture.dir.path(),
        &[
            "run",
            "api",
            "--env",
            "dev",
            "--var",
            "name=nobody",
            "--junit",
            report.to_str().unwrap(),
        ],
    );
    assert_eq!(output.code, 1, "{}", output.stdout);
    let xml = std::fs::read_to_string(report).unwrap();
    assert!(
        xml.contains("<testsuite name=\"Demo\" tests=\"3\" failures=\"1\" errors=\"0\""),
        "{xml}"
    );
    assert!(
        xml.contains("<testcase name=\"api/1-login\" classname=\"cornu\""),
        "{xml}"
    );
    assert!(xml.contains("<failure message=\"status 404\">"), "{xml}");
}

#[test]
fn the_workspace_is_found_from_a_subfolder() {
    let fixture = fixture();
    let api = fixture.dir.path().join("api");
    let output = cli(&api, &["send", "3-hello", "--env", "dev", "--json"]);
    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    assert_eq!(output.json()["name"], "api/3-hello");
    drop(fixture.server);
}

#[test]
fn usage_errors_exit_with_two_and_say_what_to_do() {
    let fixture = fixture();
    let nowhere = tempfile::tempdir().unwrap();
    let output = cli(nowhere.path(), &["list"]);
    assert_eq!(output.code, 2);
    assert!(
        output.stderr.contains("no cornu workspace"),
        "{}",
        output.stderr
    );

    let output = cli(fixture.dir.path(), &["run", "--env", "prod"]);
    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr
            .contains("no environment prod, available: dev"),
        "{}",
        output.stderr
    );

    let output = cli(fixture.dir.path(), &["run", "nope", "--json"]);
    assert_eq!(output.code, 2);
    assert!(
        output.json()["error"]
            .as_str()
            .unwrap()
            .contains("no request or folder nope")
    );

    let output = cli(fixture.dir.path(), &["run", "--var", "oops"]);
    assert_eq!(output.code, 2);
    assert!(output.stderr.contains("NAME=VALUE"), "{}", output.stderr);
}

#[test]
fn scripts_print_and_test_in_both_outputs() {
    let fixture = fixture();
    let workspace = Workspace::load(fixture.dir.path()).unwrap();
    let mut request = Request::new(Method::Get, "{{base}}/hello/{{name}}").with_name("Scripted");
    request.scripts = cornu_core::Scripts {
        pre: "vars.name = \"bob\";".into(),
        post: "print(`got ${response.body}`);\ntest(\"greets bob\", response.body == \"hi bob\");\ntest(\"greets ada\", response.body == \"hi ada\");".into(),
    };
    workspace
        .save_request(&RequestEntry {
            path: "api/4-scripted.req.toml".into(),
            request,
        })
        .unwrap();

    let output = cli(
        fixture.dir.path(),
        &["run", "api/4-scripted", "--env", "dev"],
    );
    assert_eq!(output.code, 1, "{}", output.stdout);
    for expected in [
        "✘ 200  GET     api/4-scripted",
        "› got hi bob",
        "✘ greets ada: failed",
    ] {
        assert!(
            output.stdout.contains(expected),
            "{expected} in {}",
            output.stdout
        );
    }
    let json = cli(
        fixture.dir.path(),
        &["send", "api/4-scripted", "--env", "dev", "--json"],
    )
    .json();
    assert_eq!(json["logs"], serde_json::json!(["got hi bob"]));
    assert_eq!(
        json["captures"][0],
        serde_json::json!({"variable": "name", "value": "bob"})
    );
    assert_eq!(json["assertions"][0]["passed"], true);
    assert_eq!(json["assertions"][1]["passed"], false);
}
