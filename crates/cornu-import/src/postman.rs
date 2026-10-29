//! Postman collection format v2.0 / v2.1: <https://schema.postman.com/collection/json/v2.1.0/draft-07/docs/index.html>

use cornu_core::{
    ApiKeyLocation, Auth, Body, Environment, KeyValue, Method, MultipartPart, Request,
};
use serde_json::{Map, Value};

use crate::{Collection, ImportError, ImportedFolder, ImportedItem};

pub fn parse_collection(json: &str) -> Result<Collection, ImportError> {
    let value: Value = serde_json::from_str(json)?;
    let object = value
        .as_object()
        .ok_or_else(|| ImportError::Unsupported("not a Postman collection".into()))?;
    // The Postman API wraps collections in `{"collection": {...}}`.
    let object = object
        .get("collection")
        .and_then(Value::as_object)
        .unwrap_or(object);
    let Some(info) = object.get("info").and_then(Value::as_object) else {
        let message = if object.contains_key("requests") {
            "Postman v1 collections are not supported: export the collection again as v2.1"
        } else {
            "not a Postman collection (no \"info\" section)"
        };
        return Err(ImportError::Unsupported(message.into()));
    };

    let mut importer = Importer::default();
    let name = string(info.get("name")).unwrap_or_else(|| "Imported collection".into());
    let root = importer.folder(object, name, "");
    Ok(Collection {
        root,
        warnings: importer.finish(),
    })
}

/// Also accepts globals exports, which share the environment format.
pub fn parse_environment(json: &str) -> Result<Environment, ImportError> {
    let value: Value = serde_json::from_str(json)?;
    let object = value
        .as_object()
        .ok_or_else(|| ImportError::Unsupported("not a Postman environment".into()))?;
    let object = object
        .get("environment")
        .and_then(Value::as_object)
        .unwrap_or(object);
    if object.contains_key("info") {
        return Err(ImportError::Unsupported(
            "this is a Postman collection, not an environment".into(),
        ));
    }
    let Some(values) = object.get("values").and_then(Value::as_array) else {
        return Err(ImportError::Unsupported(
            "not a Postman environment (no \"values\" list)".into(),
        ));
    };

    let mut environment = Environment::new(
        string(object.get("name")).unwrap_or_else(|| "Imported environment".into()),
    );
    environment.variables = values
        .iter()
        .filter_map(|row| {
            let key = string(row.get("key"))?;
            let mut kv = KeyValue::new(key, string(row.get("value")).unwrap_or_default());
            kv.enabled = row.get("enabled").and_then(Value::as_bool) != Some(false);
            kv.secret = row.get("type").and_then(Value::as_str) == Some("secret");
            Some(kv)
        })
        .collect();
    Ok(environment)
}

#[derive(Default)]
struct Importer {
    warnings: Vec<String>,
    scripts: usize,
    examples: usize,
}

impl Importer {
    fn finish(mut self) -> Vec<String> {
        if self.scripts > 0 {
            self.warnings.push(format!(
                "{} pre-request or test script(s) skipped: cornu does not run Postman scripts",
                self.scripts
            ));
        }
        if self.examples > 0 {
            self.warnings.push(format!(
                "{} saved example response(s) skipped",
                self.examples
            ));
        }
        self.warnings
    }

    fn folder(
        &mut self,
        object: &Map<String, Value>,
        name: String,
        parent: &str,
    ) -> ImportedFolder {
        let location = join(parent, &name);
        self.count_scripts(object);
        let items = object
            .get("item")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .filter_map(|item| {
                let name = string(item.get("name"));
                if item.contains_key("item") {
                    let name = name.unwrap_or_else(|| "Untitled folder".into());
                    Some(ImportedItem::Folder(self.folder(item, name, &location)))
                } else if item.contains_key("request") {
                    let name = name.unwrap_or_default();
                    Some(ImportedItem::Request(self.request(item, name, &location)))
                } else {
                    None
                }
            })
            .collect();
        ImportedFolder {
            variables: key_values(object.get("variable")),
            auth: self.auth(object.get("auth"), &location),
            name,
            items,
        }
    }

    fn request(&mut self, item: &Map<String, Value>, name: String, parent: &str) -> Request {
        let location = join(parent, &name);
        self.count_scripts(item);
        self.examples += item
            .get("response")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);

        let request = match item.get("request") {
            Some(Value::String(url)) => {
                let (url, query) = split_query(url);
                return Request {
                    query,
                    ..Request::new(Method::Get, url).with_name(name)
                };
            }
            Some(Value::Object(request)) => request,
            _ => return Request::new(Method::Get, "").with_name(name),
        };
        let method = string(request.get("method"))
            .and_then(|method| method.parse().ok())
            .unwrap_or_default();
        let (url, query) = url(request.get("url"));
        let headers = headers(request.get("header"));
        let body = self.body(request.get("body"), &headers, &location);
        Request {
            name,
            method,
            url,
            query,
            auth: self.auth(request.get("auth"), &location),
            headers,
            body,
        }
    }

    fn count_scripts(&mut self, object: &Map<String, Value>) {
        let has_code = |event: &&Value| {
            let exec = event.get("script").and_then(|script| script.get("exec"));
            match exec {
                Some(Value::String(code)) => !code.trim().is_empty(),
                Some(Value::Array(lines)) => lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|line| !line.trim().is_empty())),
                _ => false,
            }
        };
        self.scripts += object
            .get("event")
            .and_then(Value::as_array)
            .map_or(0, |events| events.iter().filter(has_code).count());
    }

    fn auth(&mut self, value: Option<&Value>, location: &str) -> Auth {
        let Some(auth) = value.and_then(Value::as_object) else {
            return Auth::Inherit;
        };
        let kind = auth
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("inherit");
        let param = |name: &str| auth_param(auth.get(kind), name);
        match kind {
            "inherit" => Auth::Inherit,
            "noauth" => Auth::None,
            "bearer" => Auth::Bearer {
                token: param("token"),
            },
            "basic" => Auth::Basic {
                username: param("username"),
                password: param("password"),
            },
            "apikey" => Auth::ApiKey {
                key: param("key"),
                value: param("value"),
                location: if param("in") == "query" {
                    ApiKeyLocation::Query
                } else {
                    ApiKeyLocation::Header
                },
            },
            other => {
                self.warnings.push(format!(
                    "{}: {other} auth is not supported yet, imported as no auth",
                    location
                ));
                Auth::None
            }
        }
    }

    fn body(&mut self, value: Option<&Value>, headers: &[KeyValue], location: &str) -> Body {
        let Some(body) = value.and_then(Value::as_object) else {
            return Body::None;
        };
        if body.get("disabled").and_then(Value::as_bool) == Some(true) {
            return Body::None;
        }
        match body.get("mode").and_then(Value::as_str) {
            Some("raw") => {
                let text = string(body.get("raw")).unwrap_or_default();
                let language = body
                    .get("options")
                    .and_then(|options| options.get("raw"))
                    .and_then(|raw| raw.get("language"))
                    .and_then(Value::as_str);
                let header = headers
                    .iter()
                    .find(|row| row.enabled && row.key.eq_ignore_ascii_case("content-type"))
                    .map(|row| row.value.clone());
                let is_json = language == Some("json")
                    || header
                        .as_deref()
                        .is_some_and(|value| value.contains("json"));
                if is_json {
                    Body::Json { text }
                } else {
                    let content_type = header.unwrap_or_else(|| {
                        match language {
                            Some("xml") => "application/xml",
                            Some("html") => "text/html",
                            Some("javascript") => "application/javascript",
                            _ => "text/plain",
                        }
                        .into()
                    });
                    Body::Raw { text, content_type }
                }
            }
            Some("urlencoded") => Body::FormUrlEncoded {
                fields: key_values(body.get("urlencoded")),
            },
            Some("formdata") => Body::Multipart {
                parts: form_parts(body.get("formdata")),
            },
            Some("file") => Body::Binary {
                path: body
                    .get("file")
                    .and_then(|file| string(file.get("src")))
                    .unwrap_or_default()
                    .into(),
                content_type: None,
            },
            Some("graphql") => {
                let graphql = body.get("graphql");
                let query = graphql
                    .and_then(|graphql| string(graphql.get("query")))
                    .unwrap_or_default();
                let variables = match graphql.and_then(|graphql| graphql.get("variables")) {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(text)) => text.trim().to_owned(),
                    Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                Body::GraphQl { query, variables }
            }
            Some(other) => {
                self.warnings.push(format!(
                    "{}: {other} body is not supported, imported without a body",
                    location
                ));
                Body::None
            }
            None => Body::None,
        }
    }
}

fn url(value: Option<&Value>) -> (String, Vec<KeyValue>) {
    let url = match value {
        Some(Value::String(raw)) => return split_query(raw),
        Some(Value::Object(url)) => url,
        _ => return (String::new(), Vec::new()),
    };
    let (mut base, raw_query) = match string(url.get("raw")) {
        Some(raw) => split_query(&raw),
        None => (build_url(url), Vec::new()),
    };
    let query = match url.get("query") {
        Some(query @ Value::Array(_)) => key_values(Some(query)),
        _ => raw_query,
    };
    // Path variables (`/users/:id`) have no equivalent yet, so their value is inlined.
    for variable in key_values(url.get("variable")) {
        if !variable.value.is_empty() {
            base = replace_segment(&base, &format!(":{}", variable.key), &variable.value);
        }
    }
    (base, query)
}

fn build_url(url: &Map<String, Value>) -> String {
    let joined = |value: Option<&Value>, separator: &str| match value {
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => string(part.get("value")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(separator),
        other => string(other).unwrap_or_default(),
    };
    let mut result = String::new();
    if let Some(protocol) = string(url.get("protocol")) {
        result.push_str(&protocol);
        result.push_str("://");
    }
    result.push_str(&joined(url.get("host"), "."));
    if let Some(port) = string(url.get("port")) {
        result.push(':');
        result.push_str(&port);
    }
    let path = joined(url.get("path"), "/");
    if !path.is_empty() {
        result.push('/');
        result.push_str(path.trim_start_matches('/'));
    }
    result
}

/// Postman keeps the query in the raw URL unencoded, so it is split without decoding.
fn split_query(raw: &str) -> (String, Vec<KeyValue>) {
    let raw = raw.split_once('#').map_or(raw, |(before, _)| before);
    let Some((base, query)) = raw.split_once('?') else {
        return (raw.to_owned(), Vec::new());
    };
    let rows = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => KeyValue::new(key, value),
            None => KeyValue::new(pair, ""),
        })
        .collect();
    (base.to_owned(), rows)
}

fn replace_segment(url: &str, segment: &str, value: &str) -> String {
    url.split('/')
        .map(|part| if part == segment { value } else { part })
        .collect::<Vec<_>>()
        .join("/")
}

fn headers(value: Option<&Value>) -> Vec<KeyValue> {
    match value {
        // Postman v2.0 allowed headers as a "Key: Value" block.
        Some(Value::String(block)) => block
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| KeyValue::new(key.trim(), value.trim()))
            .collect(),
        other => key_values(other),
    }
}

fn form_parts(value: Option<&Value>) -> Vec<MultipartPart> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|field| {
            let name = string(field.get("key")).unwrap_or_default();
            let mut part = if field.get("type").and_then(Value::as_str) == Some("file") {
                let src = match field.get("src") {
                    Some(Value::Array(sources)) => string(sources.first()),
                    other => string(other),
                };
                MultipartPart::file(name, src.unwrap_or_default())
            } else {
                MultipartPart::text(name, string(field.get("value")).unwrap_or_default())
            };
            part.content_type = string(field.get("contentType")).filter(|value| !value.is_empty());
            part.enabled = !disabled(field);
            part
        })
        .collect()
}

fn key_values(value: Option<&Value>) -> Vec<KeyValue> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let key = string(row.get("key"))?;
            let mut kv = KeyValue::new(key, string(row.get("value")).unwrap_or_default());
            kv.enabled = !disabled(row);
            kv.secret = row.get("type").and_then(Value::as_str) == Some("secret");
            kv.description = match row.get("description") {
                Some(Value::Object(description)) => string(description.get("content")),
                other => string(other),
            }
            .filter(|description| !description.is_empty());
            Some(kv)
        })
        .collect()
}

/// v2.1 stores auth parameters as `[{"key", "value"}]`, v2.0 as a plain object.
fn auth_param(params: Option<&Value>, name: &str) -> String {
    let value = match params {
        Some(Value::Array(rows)) => rows
            .iter()
            .find(|row| row.get("key").and_then(Value::as_str) == Some(name))
            .and_then(|row| row.get("value")),
        Some(Value::Object(map)) => map.get(name),
        _ => None,
    };
    string(value).unwrap_or_default()
}

fn disabled(row: &Value) -> bool {
    row.get("disabled").and_then(Value::as_bool) == Some(true)
}

/// Postman values are usually strings, but numbers and booleans show up in exports too.
fn string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent} / {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_query_is_split_without_decoding() {
        let (base, query) = split_query("{{host}}/search?q=a b&flag&empty=#top");
        assert_eq!(base, "{{host}}/search");
        assert_eq!(
            query,
            [
                KeyValue::new("q", "a b"),
                KeyValue::new("flag", ""),
                KeyValue::new("empty", "")
            ]
        );
    }

    #[test]
    fn url_is_rebuilt_from_parts_when_raw_is_missing() {
        let url: Value = serde_json::json!({
            "protocol": "https",
            "host": ["api", "example", "com"],
            "port": 8443,
            "path": ["v1", "users"]
        });
        assert_eq!(
            build_url(url.as_object().unwrap()),
            "https://api.example.com:8443/v1/users"
        );
    }

    #[test]
    fn path_variables_only_replace_whole_segments() {
        assert_eq!(
            replace_segment("/users/:id/:idx", ":id", "42"),
            "/users/42/:idx"
        );
    }
}
