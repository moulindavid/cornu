use std::time::Duration;

use cornu_core::{
    AssertSource, Assertion, Method, Operator, Request, Response, Variables, check, passed,
};

fn response(status: u16) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: br#"{"id":7,"name":"Ada Lovelace","tags":["a","b"],"admin":false}"#.to_vec(),
        elapsed: Duration::from_millis(120),
    }
}

fn error(assertion: Assertion) -> Option<String> {
    assertion.check(&response(200)).error
}

#[test]
fn operators_compare_text_and_numbers() {
    use Operator::*;
    for (path, operator, expected) in [
        ("$.id", Equals, "7"),
        ("$.name", Equals, "Ada Lovelace"),
        ("$.name", NotEquals, "Grace"),
        ("$.name", Contains, "Love"),
        ("$.name", NotContains, "Hopper"),
        ("$.tags", Contains, "\"b\""),
        ("$.admin", Equals, "false"),
        ("$.id", LessThan, "7.5"),
        ("$.id", LessOrEqual, "7"),
        ("$.id", GreaterThan, "-1"),
        ("$.id", GreaterOrEqual, " 7 "),
        ("$.id", Exists, ""),
        ("$.email", NotExists, ""),
    ] {
        let assertion = Assertion::body(path, operator, expected);
        assert_eq!(error(assertion.clone()), None, "{assertion}");
    }
}

#[test]
fn failures_explain_what_was_found() {
    assert_eq!(
        error(Assertion::body("$.id", Operator::Equals, "8")).as_deref(),
        Some("got 7")
    );
    assert_eq!(
        error(Assertion::body("$.email", Operator::Exists, "")).as_deref(),
        Some("missing")
    );
    assert_eq!(
        error(Assertion::body("$.id", Operator::NotExists, "")).as_deref(),
        Some("found 7")
    );
    assert_eq!(
        error(Assertion::body("$.email", Operator::Equals, "x")).as_deref(),
        Some("nothing matches $.email")
    );
    assert!(
        error(Assertion::body("$.name", Operator::GreaterThan, "1"))
            .unwrap()
            .contains("as numbers")
    );
}

#[test]
fn status_headers_time_and_size() {
    let header = Assertion::new(
        AssertSource::Header,
        "content-type",
        Operator::Contains,
        "json",
    );
    let time = Assertion::new(AssertSource::Duration, "", Operator::LessThan, "500");
    let size = Assertion::new(AssertSource::Size, "", Operator::GreaterThan, "10");
    for assertion in [Assertion::status(200), header, time, size] {
        assert_eq!(error(assertion.clone()), None, "{assertion}");
    }
    let result = Assertion::status(201).check(&response(200));
    assert_eq!(result.description, "status equals 201");
    assert_eq!(result.actual.as_deref(), Some("200"));
    assert!(!result.passed());
}

#[test]
fn assertions_decide_success_over_the_status() {
    let mut disabled = Assertion::status(500);
    disabled.enabled = false;
    let expect_404 = [Assertion::status(404), disabled];
    let results = check(&expect_404, &response(404));
    assert_eq!(results.len(), 1);
    assert!(passed(&response(404), &results));
    assert!(!passed(&response(200), &check(&expect_404, &response(200))));
    assert!(passed(&response(204), &[]));
    assert!(!passed(&response(404), &[]));
}

#[test]
fn expected_values_are_resolved_with_the_request() {
    let mut disabled = Assertion::body("$.x", Operator::Equals, "{{off}}");
    disabled.enabled = false;
    let request = Request::new(Method::Get, "/")
        .with_assertion(Assertion::body("$.id", Operator::Equals, "{{id}}"))
        .with_assertion(disabled);
    let resolved = request.resolve(&Variables::new().with_layer([("id", "7")]));
    assert_eq!(resolved.request.assertions[0].expected, "7");
    assert_eq!(resolved.request.assertions[1].expected, "{{off}}");
    assert!(resolved.unresolved.is_empty());
}

#[test]
fn assertions_are_stored_compactly_in_toml() {
    let request = Request::default()
        .with_assertion(Assertion::status(200))
        .with_assertion(Assertion::body("$.id", Operator::Exists, ""));
    let text = toml::to_string(&request).unwrap();
    assert!(
        text.contains(
            "[[assertions]]\nsource = \"status\"\noperator = \"equals\"\nexpected = \"200\"\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "[[assertions]]\nsource = \"body\"\nexpression = \"$.id\"\noperator = \"exists\"\n"
        ),
        "{text}"
    );
    assert_eq!(toml::from_str::<Request>(&text).unwrap(), request);
}
