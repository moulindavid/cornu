use std::cell::RefCell;
use std::rc::Rc;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use rhai::{Dynamic, Engine, ImmutableString, Map, Scope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AssertionResult, Body, KeyValue, Method, Request, Response, Variables};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scripts {
    /// Runs before variables are resolved: it may change the request and set variables.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pre: String,
    /// Runs on the response: it may set variables and add tests.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub post: String,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.pre.trim().is_empty() && self.post.trim().is_empty()
    }
}

/// The scripts of a request or of one of its folders, which run first, outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedScripts {
    /// Where the scripts come from, to tell which one failed.
    pub origin: String,
    pub scripts: Scripts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptOutput {
    /// Set through `vars`, in the order they were first set.
    pub variables: Vec<(String, String)>,
    pub tests: Vec<AssertionResult>,
    pub logs: Vec<String>,
    /// The first error, which stops the scripts.
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    base: Variables,
    set: Vec<(String, String)>,
    tests: Vec<AssertionResult>,
    logs: Vec<String>,
}

impl State {
    fn get(&self, name: &str) -> Option<String> {
        self.set
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| self.base.get(name).map(str::to_owned))
    }

    fn set(&mut self, name: &str, value: String) {
        match self.set.iter_mut().find(|(key, _)| key == name) {
            Some((_, old)) => *old = value,
            None => self.set.push((name.to_owned(), value)),
        }
    }

    fn variables(&self) -> Variables {
        self.base.clone().with_layer(self.set.iter().cloned())
    }
}

type Shared = Rc<RefCell<State>>;

/// `vars.name` and `vars["name"]` read and set variables.
#[derive(Clone)]
struct Vars(Shared);

const MAX_OPERATIONS: u64 = 10_000_000;

fn engine(state: &Shared) -> Engine {
    let mut engine = Engine::new();
    engine
        .set_max_operations(MAX_OPERATIONS)
        .set_max_call_levels(64)
        .set_max_expr_depths(128, 64)
        .set_max_string_size(16 << 20)
        .set_max_array_size(1_000_000)
        .set_max_map_size(1_000_000);
    engine.disable_symbol("eval");

    let logs = state.clone();
    engine.on_print(move |text| logs.borrow_mut().logs.push(text.to_owned()));
    let logs = state.clone();
    engine.on_debug(move |text, _, _| logs.borrow_mut().logs.push(text.to_owned()));

    engine
        .register_type_with_name::<Vars>("Vars")
        .register_indexer_get(|vars: &mut Vars, name: ImmutableString| {
            vars.0
                .borrow()
                .get(&name)
                .map_or(Dynamic::UNIT, Dynamic::from)
        })
        .register_indexer_set(|vars: &mut Vars, name: ImmutableString, value: Dynamic| {
            vars.0.borrow_mut().set(&name, text(value));
        });

    let resolve = state.clone();
    engine.register_fn("resolve", move |text: &str| {
        resolve.borrow().variables().interpolate(text).text
    });
    let tests = state.clone();
    engine.register_fn("test", move |name: &str, passed: bool| {
        tests.borrow_mut().tests.push(AssertionResult {
            description: name.to_owned(),
            actual: None,
            error: (!passed).then(|| "failed".to_owned()),
        });
    });

    engine
        .register_fn("sha256", |text: &str| hex(&Sha256::digest(text.as_bytes())))
        .register_fn("hmac_sha256", |key: &str, message: &str| {
            hex(&hmac_sha256(key, message))
        })
        .register_fn("hmac_sha256_base64", |key: &str, message: &str| {
            base64::engine::general_purpose::STANDARD.encode(hmac_sha256(key, message))
        })
        .register_fn("base64_encode", |text: &str| {
            base64::engine::general_purpose::STANDARD.encode(text)
        })
        .register_fn(
            "base64_decode",
            |text: &str| -> Result<String, Box<rhai::EvalAltResult>> {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(text.trim())
                    .map_err(|error| error.to_string())?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            },
        )
        .register_fn("url_encode", |text: &str| {
            url::form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>()
        })
        .register_fn("uuid", || uuid::Uuid::new_v4().to_string())
        .register_fn("timestamp", || {
            time::OffsetDateTime::now_utc().unix_timestamp()
        })
        .register_fn("timestamp_ms", || {
            (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
        })
        .register_fn("now_iso", || {
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        });
    engine
}

fn hmac_sha256(key: &str, message: &str) -> Vec<u8> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any size");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Strings as they are, anything else as JSON.
fn text(value: Dynamic) -> String {
    if value.is_string() {
        return value.into_string().unwrap_or_default();
    }
    match rhai::serde::from_dynamic::<serde_json::Value>(&value) {
        Ok(serde_json::Value::Null) => String::new(),
        Ok(json) => json.to_string(),
        Err(_) => value.to_string(),
    }
}

fn rows(rows: &[KeyValue]) -> Map {
    crate::key_value::enabled(rows)
        .map(|(key, value)| (key.into(), Dynamic::from(value.to_owned())))
        .collect()
}

/// Rows kept, changed or removed in place, new ones appended; disabled rows are left alone.
fn apply_rows(rows: &mut Vec<KeyValue>, map: &Map) {
    let mut seen = Vec::new();
    rows.retain_mut(|row| {
        if !row.enabled {
            return true;
        }
        match map.get(row.key.as_str()) {
            Some(value) if !seen.contains(&row.key) => {
                seen.push(row.key.clone());
                row.value = text(value.clone());
                true
            }
            _ => false,
        }
    });
    for (key, value) in map {
        if !seen.iter().any(|seen| seen == key.as_str()) {
            rows.push(KeyValue::new(key.as_str(), text(value.clone())));
        }
    }
}

fn body_text(body: &Body) -> &str {
    match body {
        Body::Raw { text, .. } | Body::Json { text } => text,
        _ => "",
    }
}

fn request_map(request: &Request) -> Map {
    let mut map = Map::new();
    map.insert("name".into(), request.name.clone().into());
    map.insert("method".into(), request.method.as_str().into());
    map.insert("url".into(), request.url.clone().into());
    map.insert("headers".into(), rows(&request.headers).into());
    map.insert("query".into(), rows(&request.query).into());
    map.insert("body".into(), body_text(&request.body).into());
    map
}

fn apply_request(request: &mut Request, map: &Map) -> Result<(), String> {
    let field = |name: &str| map.get(name).cloned().unwrap_or(Dynamic::UNIT);
    let method = text(field("method"));
    request.method = method
        .parse::<Method>()
        .map_err(|_| format!("invalid method {method:?}"))?;
    request.url = text(field("url"));
    for (name, rows) in [
        ("headers", &mut request.headers),
        ("query", &mut request.query),
    ] {
        let value = field(name);
        let Some(map) = value.try_cast::<Map>() else {
            return Err(format!("request.{name} must stay a map"));
        };
        apply_rows(rows, &map);
    }
    let body = text(field("body"));
    if body != body_text(&request.body) {
        match &mut request.body {
            Body::Raw { text, .. } | Body::Json { text } => *text = body,
            Body::None if serde_json::from_str::<serde_json::Value>(&body).is_ok() => {
                request.body = Body::Json { text: body };
            }
            Body::None => {
                request.body = Body::Raw {
                    text: body,
                    content_type: "text/plain".into(),
                }
            }
            _ => return Err("the script can only change text and JSON bodies".into()),
        }
    }
    Ok(())
}

fn response_map(response: &Response) -> Map {
    let mut headers = Map::new();
    for (name, value) in &response.headers {
        let name = name.to_ascii_lowercase();
        let joined = match headers.get(name.as_str()) {
            Some(old) => format!("{}, {value}", old.clone().into_string().unwrap_or_default()),
            None => value.clone(),
        };
        headers.insert(name.into(), joined.into());
    }
    let body = response.text().into_owned();
    let json = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| rhai::serde::to_dynamic(value).ok())
        .unwrap_or(Dynamic::UNIT);
    let mut map = Map::new();
    map.insert("status".into(), (response.status as i64).into());
    map.insert("headers".into(), headers.into());
    map.insert("body".into(), body.into());
    map.insert("json".into(), json);
    map.insert("time".into(), (response.elapsed.as_millis() as i64).into());
    map.insert("size".into(), (response.size() as i64).into());
    map
}

fn run(
    stage: &str,
    scripts: &[&NamedScripts],
    source: impl Fn(&Scripts) -> &str,
    variables: &Variables,
    scope: &mut Scope,
) -> ScriptOutput {
    let state = Rc::new(RefCell::new(State {
        base: variables.clone(),
        ..Default::default()
    }));
    let engine = engine(&state);
    scope.push("vars", Vars(state.clone()));
    let mut error = None;
    for named in scripts {
        let code = source(&named.scripts);
        if code.trim().is_empty() {
            continue;
        }
        if let Err(failure) = engine.run_with_scope(scope, code) {
            error = Some(format!("{stage} script of {}: {failure}", named.origin));
            break;
        }
    }
    drop(engine);
    let state = Rc::try_unwrap(state)
        .map(RefCell::into_inner)
        .unwrap_or_else(|state| std::mem::take(&mut *state.borrow_mut()));
    ScriptOutput {
        variables: state.set,
        tests: state.tests,
        logs: state.logs,
        error,
    }
}

/// Before variables are resolved, so the request still holds its `{{placeholders}}`;
/// `resolve(text)` gives their values.
pub fn pre_request(
    scripts: &[&NamedScripts],
    request: &mut Request,
    variables: &Variables,
) -> ScriptOutput {
    let mut scope = Scope::new();
    scope.push("request", request_map(request));
    let mut output = run("pre-request", scripts, |s| &s.pre, variables, &mut scope);
    if output.error.is_none() {
        let applied = match scope.get_value::<Map>("request") {
            Some(map) => apply_request(request, &map),
            None => Err("request must stay a map".into()),
        };
        if let Err(error) = applied {
            output.error = Some(format!("pre-request script: {error}"));
        }
    }
    output
}

pub fn post_response(
    scripts: &[&NamedScripts],
    request: &Request,
    response: &Response,
    variables: &Variables,
) -> ScriptOutput {
    let mut scope = Scope::new();
    scope.push_constant("request", request_map(request));
    scope.push_constant("response", response_map(response));
    run("post-response", scripts, |s| &s.post, variables, &mut scope)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn named(pre: &str, post: &str) -> NamedScripts {
        NamedScripts {
            origin: "request".into(),
            scripts: Scripts {
                pre: pre.into(),
                post: post.into(),
            },
        }
    }

    #[test]
    fn pre_request_scripts_change_the_request_and_set_variables() {
        let mut request = Request::new(Method::Get, "{{base}}/items")
            .with_header("X-Old", "1")
            .with_header("X-Keep", "2")
            .with_query("page", "1");
        let variables =
            Variables::new().with_layer([("base", "https://api.example"), ("secret", "k")]);
        let script = r#"
            let url = resolve(request.url);
            vars.signature = hmac_sha256(vars.secret, request.method + url);
            vars["count"] = 3;
            request.method = "POST";
            request.headers.remove("X-Old");
            request.headers["X-Signature"] = "{{signature}}";
            request.query.page = 2;
            request.body = `{"n": ${vars.count}}`;
            print(`signed ${url}`);
        "#;
        let output = pre_request(&[&named(script, "")], &mut request, &variables);
        assert_eq!(output.error, None);
        let expected = hex(&hmac_sha256("k", "GEThttps://api.example/items"));
        assert_eq!(
            output.variables,
            [("signature".into(), expected), ("count".into(), "3".into())]
        );
        assert_eq!(output.logs, ["signed https://api.example/items"]);
        assert_eq!(request.method, Method::Post);
        let headers: Vec<_> = crate::key_value::enabled(&request.headers).collect();
        assert_eq!(headers, [("X-Keep", "2"), ("X-Signature", "{{signature}}")]);
        assert_eq!(request.query[0].value, "2");
        assert_eq!(
            request.body,
            Body::Json {
                text: "{\"n\": 3}".into()
            }
        );
    }

    #[test]
    fn post_response_scripts_read_the_response_and_add_tests() {
        let response = Response {
            status: 201,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"data": [{"id": 4, "role": "user"}, {"id": 9, "role": "admin"}]}"#.to_vec(),
            elapsed: Duration::from_millis(12),
        };
        let script = r#"
            let users = response.json.data;
            test("created", response.status == 201);
            test("json", response.headers["content-type"].contains("json"));
            test("an admin", users.some(|u| u.role == "admin"));
            test("fast", response.time < 5);
            vars.first_id = users[0].id;
            vars.user = users[0];
        "#;
        let request = Request::new(Method::Post, "https://api.example/users");
        let output = post_response(
            &[&named("", script)],
            &request,
            &response,
            &Variables::new(),
        );
        assert_eq!(output.error, None);
        let results: Vec<_> = output
            .tests
            .iter()
            .map(|test| (test.description.as_str(), test.passed()))
            .collect();
        assert_eq!(
            results,
            [
                ("created", true),
                ("json", true),
                ("an admin", true),
                ("fast", false)
            ]
        );
        assert_eq!(output.variables[0], ("first_id".into(), "4".into()));
        assert_eq!(output.variables[1].1, r#"{"id":4,"role":"user"}"#);
    }

    #[test]
    fn errors_name_the_script_and_stop_the_chain() {
        let folder = NamedScripts {
            origin: "folder Users".into(),
            scripts: Scripts {
                pre: "vars.a = 1;\nnope();".into(),
                post: String::new(),
            },
        };
        let mut request = Request::new(Method::Get, "x");
        let output = pre_request(
            &[&folder, &named("vars.b = 2;", "")],
            &mut request,
            &Variables::new(),
        );
        let error = output.error.unwrap();
        assert!(
            error.starts_with("pre-request script of folder Users:"),
            "{error}"
        );
        assert!(
            error.contains("nope") && error.contains("line 2"),
            "{error}"
        );
        assert_eq!(output.variables, [("a".into(), "1".into())]);

        let output = pre_request(&[&named("loop {}", "")], &mut request, &Variables::new());
        assert!(output.error.unwrap().contains("Too many operations"));
        let output = pre_request(
            &[&named("eval(\"1\")", "")],
            &mut request,
            &Variables::new(),
        );
        assert!(output.error.is_some());
        let output = pre_request(
            &[&named("request = 1;", "")],
            &mut request,
            &Variables::new(),
        );
        assert!(output.error.unwrap().contains("must stay a map"));
    }

    #[test]
    fn helpers_encode_hash_and_date() {
        let response = Response {
            status: 200,
            headers: Vec::new(),
            body: Vec::new(),
            elapsed: Duration::ZERO,
        };
        let script = r#"
            test("sha256", sha256("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
            test("hmac", hmac_sha256("key", "The quick brown fox jumps over the lazy dog") == "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
            test("hmac base64", hmac_sha256_base64("key", "") == "XV0TlWPJW1lnub2ajJsjOp3ttFByeUzSMtwbdIMmB9A=");
            test("base64", base64_decode(base64_encode("héllo")) == "héllo");
            test("url", url_encode("a b&c") == "a+b%26c");
            test("uuid", uuid().len() == 36);
            test("time", timestamp_ms() / 1000 - timestamp() <= 1 && now_iso().contains("T"));
            test("not json", response.json == ());
        "#;
        let output = post_response(
            &[&named("", script)],
            &Request::new(Method::Get, "x"),
            &response,
            &Variables::new(),
        );
        assert_eq!(output.error, None);
        let failed: Vec<_> = output
            .tests
            .iter()
            .filter(|test| !test.passed())
            .map(|test| test.description.as_str())
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
    }
}
