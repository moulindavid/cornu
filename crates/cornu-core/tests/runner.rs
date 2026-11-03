use std::time::{Duration, Instant};

use cornu_core::runner::data::{CsvData, ValueList, rows_from_lists};
use cornu_core::runner::{
    CancelToken, Row, RunOptions, RunPlan, RunSummary, Step, StepResult, run,
};
use cornu_core::{ClientOptions, HttpClient, Method, Request, SendError, Variables};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> HttpClient {
    HttpClient::new(&ClientOptions::default()).unwrap()
}

fn row(pairs: &[(&str, &str)]) -> Row {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn step(url: String, variables: &[(&str, &str)]) -> Step {
    Step {
        request: Request::new(Method::Get, url),
        variables: Variables::new().with_layer(variables.iter().copied()),
    }
}

async fn execute(plan: RunPlan, cancel: &CancelToken) -> (RunSummary, Vec<StepResult>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let summary = run(&client(), plan, cancel, tx).await;
    let mut results = Vec::new();
    while let Ok(result) = rx.try_recv() {
        results.push(result);
    }
    results.sort_by_key(|result| (result.iteration, result.step));
    (summary, results)
}

async fn slow_server(delay: Duration) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(delay))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn each_row_runs_every_step_with_its_variables() {
    let server = MockServer::start().await;
    Mock::given(path_regex("^/users/(1|2)$"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(path("/users/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(path_regex("^/audit"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let base = server.uri();
    let plan = RunPlan {
        steps: vec![
            step(
                "{{base}}/users/{{id}}".into(),
                &[("base", &base), ("id", "default")],
            ),
            step("{{base}}/audit/{{id}}/{{other}}".into(), &[("base", &base)]),
        ],
        rows: vec![
            row(&[("id", "1")]),
            row(&[("id", "2")]),
            row(&[("id", "missing")]),
        ],
        options: RunOptions {
            concurrency: 2,
            ..Default::default()
        },
    };
    assert_eq!(plan.total(), 6);
    let (summary, results) = execute(plan, &CancelToken::default()).await;

    assert_eq!(
        (summary.succeeded, summary.failed, summary.errors),
        (5, 1, 0)
    );
    assert_eq!(summary.skipped, 0);
    assert!(!summary.cancelled);
    let urls: Vec<_> = results
        .iter()
        .map(|result| result.request.url.trim_start_matches(&base))
        .collect();
    assert_eq!(
        urls,
        [
            "/users/1",
            "/audit/1/{{other}}",
            "/users/2",
            "/audit/2/{{other}}",
            "/users/missing",
            "/audit/missing/{{other}}"
        ]
    );
    assert_eq!(results[1].unresolved, ["other"]);
    assert_eq!(results[4].outcome.as_ref().unwrap().status, 404);
}

#[tokio::test]
async fn no_rows_is_one_iteration_and_errors_are_counted() {
    let plan = RunPlan {
        steps: vec![step("http://127.0.0.1:9/{{x}}".into(), &[("x", "a")])],
        rows: Vec::new(),
        options: RunOptions::default(),
    };
    let (summary, results) = execute(plan, &CancelToken::default()).await;
    assert_eq!((summary.errors, summary.skipped), (1, 0));
    assert_eq!(results[0].request.url, "http://127.0.0.1:9/a");
    assert!(matches!(results[0].outcome, Err(SendError::Connect(_))));
}

#[tokio::test]
async fn concurrency_limits_iterations_in_flight() {
    let server = slow_server(Duration::from_millis(150)).await;
    let plan = |concurrency| RunPlan {
        steps: vec![step(server.uri(), &[])],
        rows: vec![Row::new(); 4],
        options: RunOptions {
            concurrency,
            ..Default::default()
        },
    };
    let (sequential, _) = execute(plan(1), &CancelToken::default()).await;
    assert!(
        sequential.elapsed >= Duration::from_millis(600),
        "{sequential:?}"
    );
    let (parallel, _) = execute(plan(4), &CancelToken::default()).await;
    assert_eq!(parallel.succeeded, 4);
    assert!(
        parallel.elapsed < Duration::from_millis(450),
        "{parallel:?}"
    );
}

#[tokio::test]
async fn delay_spaces_requests_but_not_after_the_last() {
    let server = slow_server(Duration::ZERO).await;
    let plan = RunPlan {
        steps: vec![step(server.uri(), &[]), step(server.uri(), &[])],
        rows: vec![Row::new(); 2],
        options: RunOptions {
            concurrency: 1,
            delay: Duration::from_millis(200),
        },
    };
    let (summary, _) = execute(plan, &CancelToken::default()).await;
    assert_eq!(summary.succeeded, 4);
    assert!(summary.elapsed >= Duration::from_millis(600), "{summary:?}");
    assert!(summary.elapsed < Duration::from_millis(780), "{summary:?}");
}

#[tokio::test]
async fn cancelling_stops_requests_in_flight_and_skips_the_rest() {
    let server = slow_server(Duration::from_secs(5)).await;
    let plan = RunPlan {
        steps: vec![step(server.uri(), &[]), step(server.uri(), &[])],
        rows: vec![Row::new(); 3],
        options: RunOptions {
            concurrency: 2,
            ..Default::default()
        },
    };
    let cancel = CancelToken::default();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });
    let started = Instant::now();
    let (summary, results) = execute(plan, &cancel).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(summary.cancelled);
    assert_eq!((summary.errors, summary.skipped), (2, 4));
    assert!(
        results
            .iter()
            .all(|result| matches!(result.outcome, Err(SendError::Cancelled)))
    );
}

#[tokio::test]
async fn a_pasted_list_drives_the_iterations() {
    let server = MockServer::start().await;
    Mock::given(path_regex("^/users/(7|8|9)$"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let plan = RunPlan {
        steps: vec![step(
            format!("{}/users/{{{{id}}}}", server.uri()),
            &[("id", "0")],
        )],
        rows: rows_from_lists(&[ValueList::new("id", "7\n8\n\n9\n")]).unwrap(),
        options: RunOptions::default(),
    };
    let (summary, results) = execute(plan, &CancelToken::default()).await;
    assert_eq!(summary.succeeded, 3);
    let ids: Vec<_> = results
        .iter()
        .map(|result| result.request.url.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(ids, ["7", "8", "9"]);
}

#[tokio::test]
async fn csv_rows_drive_the_iterations() {
    let server = MockServer::start().await;
    Mock::given(path_regex("^/users/(1|2)/(ada|grace)$"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let csv = CsvData::new("id\tname\n1\tada\n2\tgrace\n", true);
    let plan = RunPlan {
        steps: vec![step(
            format!("{}/users/{{{{id}}}}/{{{{name}}}}", server.uri()),
            &[],
        )],
        rows: csv.rows().unwrap(),
        options: RunOptions::default(),
    };
    let (summary, results) = execute(plan, &CancelToken::default()).await;
    assert_eq!(summary.succeeded, 2);
    assert!(results[1].request.url.ends_with("/users/2/grace"));
}

#[tokio::test]
async fn captures_feed_later_steps_of_the_same_iteration() {
    use cornu_core::Capture;
    use wiremock::matchers::header;

    let server = MockServer::start().await;
    Mock::given(path_regex("^/login/(alice|bob)$"))
        .respond_with(|request: &wiremock::Request| {
            let user = request.url.path().rsplit('/').next().unwrap().to_owned();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "token": format!("t-{user}") }))
        })
        .mount(&server)
        .await;
    for user in ["alice", "bob"] {
        Mock::given(path("/me"))
            .and(header("authorization", format!("Bearer t-{user}").as_str()))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }

    let base = server.uri();
    let mut login = step("{{base}}/login/{{user}}".into(), &[("base", &base)]);
    login.request = login
        .request
        .with_capture(Capture::body("token", "$.token"))
        .with_capture(Capture::body("missing", "$.nope"));
    let mut me = step("{{base}}/me".into(), &[("base", &base), ("token", "stale")]);
    me.request = me.request.with_header("Authorization", "Bearer {{token}}");
    let plan = RunPlan {
        steps: vec![login, me],
        rows: vec![row(&[("user", "alice")]), row(&[("user", "bob")])],
        options: RunOptions {
            concurrency: 2,
            ..RunOptions::default()
        },
    };

    let (summary, results) = execute(plan, &CancelToken::default()).await;
    assert_eq!(summary.succeeded, 4);
    let captured = &results[0].captured;
    assert_eq!(captured[0].value.as_deref(), Ok("t-alice"));
    assert!(captured[1].value.is_err());
    assert!(results[1].captured.is_empty());
}
