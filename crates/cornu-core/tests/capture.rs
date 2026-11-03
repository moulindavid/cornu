use std::time::Duration;

use cornu_core::{Capture, CaptureError, Captured, Request, Response, capture, store};

fn response(body: &str) -> Response {
    Response {
        status: 201,
        headers: vec![("Location".into(), "/users/7".into())],
        body: body.as_bytes().to_vec(),
        elapsed: Duration::ZERO,
    }
}

fn value(capture: Capture, body: &str) -> Result<String, CaptureError> {
    capture.extract(&response(body))
}

#[test]
fn body_paths_give_strings_unquoted_and_other_values_as_json() {
    let body = r#"{"token":"abc","user":{"id":7,"admin":false},"tags":["a","b"]}"#;
    assert_eq!(value(Capture::body("t", "$.token"), body).unwrap(), "abc");
    assert_eq!(value(Capture::body("t", "$.user.id"), body).unwrap(), "7");
    assert_eq!(
        value(Capture::body("t", "$['user']['admin']"), body).unwrap(),
        "false"
    );
    assert_eq!(
        value(Capture::body("t", "$.user"), body).unwrap(),
        r#"{"id":7,"admin":false}"#
    );
    assert_eq!(value(Capture::body("t", "$.tags[1]"), body).unwrap(), "b");
    assert_eq!(
        value(Capture::body("t", "$.tags[*]"), body).unwrap(),
        r#"["a","b"]"#
    );
    assert_eq!(value(Capture::body("t", ""), body).unwrap(), body);
}

#[test]
fn body_capture_errors() {
    assert_eq!(
        value(Capture::body("t", "$.missing"), "{}"),
        Err(CaptureError::NoMatch("$.missing".into()))
    );
    assert_eq!(
        value(Capture::body("t", "$.a"), "<html>"),
        Err(CaptureError::NotJson)
    );
    assert!(matches!(
        value(Capture::body("t", "token"), "{}"),
        Err(CaptureError::InvalidPath(_))
    ));
}

#[test]
fn headers_and_status() {
    assert_eq!(
        value(Capture::header("l", " location "), "").unwrap(),
        "/users/7"
    );
    assert_eq!(
        value(Capture::header("l", "ETag"), ""),
        Err(CaptureError::MissingHeader("ETag".into()))
    );
    assert_eq!(value(Capture::status("s"), "").unwrap(), "201");
}

#[test]
fn capture_skips_disabled_and_unnamed_and_strips_braces() {
    let mut disabled = Capture::status("off");
    disabled.enabled = false;
    let captures = [
        Capture::status(" {{ code }} "),
        disabled,
        Capture::status("  "),
    ];
    assert_eq!(
        capture(&captures, &response("")),
        [Captured {
            variable: "code".into(),
            value: Ok("201".into()),
        }]
    );
}

#[test]
fn store_replaces_existing_values_and_ignores_failures() {
    let mut variables = vec![("token".to_owned(), "old".to_owned())];
    store(
        &mut variables,
        &[
            Captured {
                variable: "token".into(),
                value: Ok("new".into()),
            },
            Captured {
                variable: "id".into(),
                value: Err(CaptureError::NotJson),
            },
            Captured {
                variable: "status".into(),
                value: Ok("200".into()),
            },
        ],
    );
    assert_eq!(
        variables,
        [
            ("token".to_owned(), "new".to_owned()),
            ("status".to_owned(), "200".to_owned())
        ]
    );
}

#[test]
fn captures_are_stored_compactly_in_toml() {
    let mut off = Capture::status("code");
    off.enabled = false;
    let request = Request::default()
        .with_capture(Capture::body("token", "$.token"))
        .with_capture(off);
    let text = toml::to_string(&request).unwrap();
    assert!(text.contains(
        "[[captures]]\nvariable = \"token\"\nsource = \"body\"\nexpression = \"$.token\"\n"
    ));
    assert!(
        text.contains("[[captures]]\nvariable = \"code\"\nsource = \"status\"\nenabled = false\n")
    );
    assert_eq!(toml::from_str::<Request>(&text).unwrap(), request);
}
