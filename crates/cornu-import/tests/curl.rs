use cornu_core::{ApiKeyLocation, Auth, Body, KeyValue, Method, MultipartPart, Request};
use cornu_import::ImportError;
use cornu_import::curl::{is_curl, parse, to_curl};

fn parsed(command: &str) -> Request {
    parse(command).unwrap().request
}

#[test]
fn chrome_copy_as_curl_is_parsed() {
    let command = r#"curl 'https://api.example.com/v1/users?page=2&q=a%20b' \
  -H 'accept: application/json' \
  -H 'authorization: Bearer eyJhbGciOi.payload.sig' \
  -H 'content-type: application/json' \
  --data-raw $'{"name":"O\'Brien","tags":["a"]}' \
  --compressed"#;
    let parsed = parse(command).unwrap();
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
    let mut expected = Request::new(Method::Post, "https://api.example.com/v1/users")
        .with_query("page", "2")
        .with_query("q", "a b")
        .with_header("accept", "application/json")
        .with_auth(Auth::Bearer {
            token: "eyJhbGciOi.payload.sig".into(),
        });
    expected.body = Body::Json {
        text: r#"{"name":"O'Brien","tags":["a"]}"#.into(),
    };
    assert_eq!(parsed.request, expected);
}

#[test]
fn explicit_method_basic_auth_and_combined_flags() {
    let request = parsed(r#"curl -sSL -XDELETE -u admin:p@ss "http://localhost:8080/items/42""#);
    assert_eq!(request.method, Method::Delete);
    assert_eq!(request.url, "http://localhost:8080/items/42");
    assert_eq!(
        request.auth,
        Auth::Basic {
            username: "admin".into(),
            password: "p@ss".into()
        }
    );
}

#[test]
fn form_data_becomes_urlencoded_fields() {
    let request = parsed(
        "curl https://example.com/login -d 'user=jo%40x.io' --data-urlencode 'pass=a b&c' -d remember",
    );
    assert_eq!(request.method, Method::Post);
    assert_eq!(
        request.body,
        Body::Raw {
            text: "user=jo%40x.io&pass=a+b%26c&remember".into(),
            content_type: "application/x-www-form-urlencoded".into(),
        }
    );

    let request = parsed("curl https://example.com/login -d user=jo --data-urlencode 'pass=a b&c'");
    assert_eq!(
        request.body,
        Body::FormUrlEncoded {
            fields: vec![KeyValue::new("user", "jo"), KeyValue::new("pass", "a b&c")]
        }
    );
}

#[test]
fn get_flag_moves_data_to_query() {
    let request = parsed("curl -G https://example.com/search -d q=rust --data-urlencode 'tag=a b'");
    assert_eq!(request.method, Method::Get);
    assert!(request.body.is_none());
    assert_eq!(
        request.query,
        vec![KeyValue::new("q", "rust"), KeyValue::new("tag", "a b")]
    );
}

#[test]
fn multipart_files_and_text() {
    let request = parsed(
        r#"curl -F 'file=@"/tmp/my photo.png";type=image/png' -F name=cat --form-string 'raw=@not-a-file' https://example.com/upload"#,
    );
    let mut file = MultipartPart::file("file", "/tmp/my photo.png");
    file.content_type = Some("image/png".into());
    assert_eq!(
        request.body,
        Body::Multipart {
            parts: vec![
                file,
                MultipartPart::text("name", "cat"),
                MultipartPart::text("raw", "@not-a-file"),
            ]
        }
    );
}

#[test]
fn custom_content_type_keeps_raw_body() {
    let request = parsed(
        "curl https://example.com -H 'Content-Type: application/xml' -H 'X-Empty;' -d '<a/>'",
    );
    assert_eq!(request.headers, vec![KeyValue::new("X-Empty", "")]);
    assert_eq!(
        request.body,
        Body::Raw {
            text: "<a/>".into(),
            content_type: "application/xml".into()
        }
    );

    let request = parsed("curl https://example.com --data-binary @payload.bin");
    assert_eq!(
        request.body,
        Body::Binary {
            path: "payload.bin".into(),
            content_type: None
        }
    );
}

#[test]
fn unsupported_options_produce_warnings() {
    let parsed =
        parse("curl -k --proxy http://p:3128 --frobnicate https://a.example https://b.example")
            .unwrap();
    assert_eq!(parsed.request.url, "https://a.example");
    assert_eq!(parsed.warnings.len(), 4, "{:?}", parsed.warnings);
}

#[test]
fn windows_cmd_continuations_and_errors() {
    let request = parsed("CURL.exe https://example.com ^\r\n  -H \"X-A: 1\"");
    assert_eq!(request.headers, vec![KeyValue::new("X-A", "1")]);

    assert!(matches!(
        parse("wget https://x"),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(
        parse("curl -H 'a: b'"),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(
        parse("curl 'https://x"),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(
        parse("curl https://x -X"),
        Err(ImportError::Unsupported(_))
    ));
}

#[test]
fn graphql_payloads_become_graphql_bodies() {
    let request = parsed(
        r#"curl https://countries.example/graphql -H 'content-type: application/json' --data-raw '{"query":"{ countries { code } }","variables":{"n":1},"operationName":null}'"#,
    );
    assert_eq!(
        request.body,
        Body::GraphQl {
            query: "{ countries { code } }".into(),
            variables: "{\n  \"n\": 1\n}".into(),
        }
    );
    assert!(request.headers.is_empty());

    let template =
        Request::new(Method::Post, "https://x.example/graphql").with_body(Body::GraphQl {
            query: "{ a }".into(),
            variables: "{\"id\": {{id}}}".into(),
        });
    assert!(
        to_curl(&template)
            .ends_with(r#"--data-raw '{"query":"{ a }","variables":{"id": {{id}}}}'"#)
    );
}

#[test]
fn curl_detection() {
    assert!(is_curl("  curl https://x"));
    assert!(is_curl("CURL.exe https://x"));
    assert!(is_curl("curl\n  https://x"));
    assert!(!is_curl("curly braces"));
    assert!(!is_curl("curl"));
    assert!(!is_curl("cu€l x"));
    assert!(!is_curl("https://example.com/curl"));
}

#[test]
fn export_is_readable() {
    let request = Request::new(Method::Post, "https://api.example.com/users")
        .with_query("dry run", "1")
        .with_header("X-Trace", "abc")
        .with_auth(Auth::Bearer {
            token: "t0k".into(),
        })
        .with_body(Body::Json {
            text: r#"{"name": "O'Brien"}"#.into(),
        });
    assert_eq!(
        to_curl(&request),
        r#"curl 'https://api.example.com/users?dry+run=1' \
  -H 'X-Trace: abc' \
  -H 'Authorization: Bearer t0k' \
  -H 'Content-Type: application/json' \
  --data-raw '{"name": "O'\''Brien"}'"#
    );
    assert_eq!(
        to_curl(&Request::new(Method::Head, "https://example.com")),
        "curl --head https://example.com"
    );
    assert_eq!(
        to_curl(&Request::new(Method::Delete, "https://example.com/1")),
        "curl -X DELETE https://example.com/1"
    );
}

#[test]
fn export_then_parse_round_trips() {
    let mut file = MultipartPart::file("avatar", "/tmp/a b.png");
    file.content_type = Some("image/png".into());
    let requests = [
        Request::new(Method::Get, "https://example.com/search")
            .with_query("q", "a&b")
            .with_header("Accept", "text/html"),
        Request::new(Method::Put, "https://example.com/users/1")
            .with_auth(Auth::Basic {
                username: "jo".into(),
                password: "it's".into(),
            })
            .with_body(Body::Json {
                text: "{\n  \"a\": 1\n}".into(),
            }),
        Request::new(Method::Post, "https://example.com/form").with_body(Body::FormUrlEncoded {
            fields: vec![KeyValue::new("a", "1 2"), KeyValue::new("b", "x=y")],
        }),
        Request::new(Method::Post, "https://example.com/upload").with_body(Body::Multipart {
            parts: vec![MultipartPart::text("note", "@hi"), file],
        }),
        Request::new(Method::Patch, "https://example.com/doc").with_body(Body::Raw {
            text: "<a>'</a>".into(),
            content_type: "application/xml".into(),
        }),
        Request::new(Method::Post, "https://example.com/graphql").with_body(Body::GraphQl {
            query: "query Q($id: ID!) {\n  user(id: $id) { name }\n}".into(),
            variables: "{\n  \"id\": \"42\"\n}".into(),
        }),
        Request::new(Method::Post, "https://example.com/blob").with_body(Body::Binary {
            path: "data.bin".into(),
            content_type: Some("application/pdf".into()),
        }),
    ];
    for request in requests {
        let command = to_curl(&request);
        let parsed = parse(&command).unwrap();
        assert!(
            parsed.warnings.is_empty(),
            "{command}: {:?}",
            parsed.warnings
        );
        assert_eq!(parsed.request, request, "{command}");
    }
}

#[test]
fn export_puts_api_key_where_it_belongs() {
    let request = Request::new(Method::Get, "https://example.com?x=1").with_auth(Auth::ApiKey {
        key: "api_key".into(),
        value: "k".into(),
        location: ApiKeyLocation::Query,
    });
    assert_eq!(
        to_curl(&request),
        "curl 'https://example.com?x=1&api_key=k'"
    );
}
