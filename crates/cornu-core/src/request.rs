use serde::{Deserialize, Serialize};

use crate::{Body, KeyValue, Method};

/// A request as authored by the user, before variables are resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub name: String,
    #[serde(default)]
    pub method: Method,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Body::is_none")]
    pub body: Body,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            ..Default::default()
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push(KeyValue::new(key, value));
        self
    }

    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push(KeyValue::new(key, value));
        self
    }

    pub fn with_body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    /// Whether an enabled header with this name (case-insensitive) is set.
    pub fn has_header(&self, name: &str) -> bool {
        crate::key_value::enabled(&self.headers).any(|(key, _)| key.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_request_serializes_compactly() {
        let request = Request::new(Method::Get, "https://example.com").with_name("home");
        let text = toml::to_string(&request).unwrap();
        assert_eq!(
            text,
            "name = \"home\"\nmethod = \"GET\"\nurl = \"https://example.com\"\n"
        );
    }

    #[test]
    fn full_request_round_trips_through_toml() {
        let request = Request::new(Method::Post, "{{base_url}}/users")
            .with_name("create user")
            .with_query("dry_run", "true")
            .with_header("X-Trace", "1")
            .with_body(Body::Json {
                text: r#"{"name": "{{name}}"}"#.into(),
            });
        let text = toml::to_string(&request).unwrap();
        assert_eq!(toml::from_str::<Request>(&text).unwrap(), request);
    }

    #[test]
    fn has_header_ignores_case_and_disabled_rows() {
        let mut request = Request::new(Method::Get, "/").with_header("Content-Type", "a");
        assert!(request.has_header("content-type"));
        request.headers[0].enabled = false;
        assert!(!request.has_header("content-type"));
    }
}
