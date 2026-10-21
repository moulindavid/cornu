use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Auth, Body, KeyValue, Method, PartValue, Variables};

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
    #[serde(default, skip_serializing_if = "Auth::is_inherit")]
    pub auth: Auth,
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

    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_body(mut self, body: Body) -> Self {
        self.body = body;
        self
    }

    pub fn has_header(&self, name: &str) -> bool {
        crate::key_value::enabled(&self.headers).any(|(key, _)| key.eq_ignore_ascii_case(name))
    }

    /// Disabled rows are left untouched and never reported as unresolved.
    pub fn resolve(&self, variables: &Variables) -> Resolved {
        let mut unresolved = Vec::new();
        let mut sub = |template: &str| {
            let result = variables.interpolate(template);
            for name in result.unresolved {
                if !unresolved.contains(&name) {
                    unresolved.push(name);
                }
            }
            result.text
        };
        let mut request = self.clone();
        request.url = sub(&request.url);
        for row in request
            .query
            .iter_mut()
            .chain(request.headers.iter_mut())
            .filter(|row| row.enabled)
        {
            row.key = sub(&row.key);
            row.value = sub(&row.value);
        }
        for field in request.auth.fields_mut() {
            *field = sub(field);
        }
        match &mut request.body {
            Body::None => {}
            Body::Raw { text, content_type } => {
                *text = sub(text);
                *content_type = sub(content_type);
            }
            Body::Json { text } => *text = sub(text),
            Body::FormUrlEncoded { fields } => {
                for field in fields.iter_mut().filter(|field| field.enabled) {
                    field.key = sub(&field.key);
                    field.value = sub(&field.value);
                }
            }
            Body::Multipart { parts } => {
                for part in parts.iter_mut().filter(|part| part.enabled) {
                    part.name = sub(&part.name);
                    match &mut part.value {
                        PartValue::Text(text) => *text = sub(text),
                        PartValue::File(path) => {
                            *path = PathBuf::from(sub(&path.to_string_lossy()))
                        }
                    }
                }
            }
            Body::Binary { path, .. } => *path = PathBuf::from(sub(&path.to_string_lossy())),
        }
        Resolved {
            request,
            unresolved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub request: Request,
    pub unresolved: Vec<String>,
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
    fn resolve_substitutes_every_sent_field() {
        let variables = Variables::new().with_layer([
            ("base", "https://api.example"),
            ("token", "secret"),
            ("id", "42"),
        ]);
        let request = Request::new(Method::Post, "{{base}}/users/{{id}}")
            .with_query("expand", "{{id}}")
            .with_header("Authorization", "Bearer {{token}}")
            .with_body(Body::Json {
                text: r#"{"id": {{id}}}"#.into(),
            });

        let resolved = request.resolve(&variables);
        assert!(resolved.unresolved.is_empty());
        let request = resolved.request;
        assert_eq!(request.url, "https://api.example/users/42");
        assert_eq!(request.query[0].value, "42");
        assert_eq!(request.headers[0].value, "Bearer secret");
        assert_eq!(
            request.body,
            Body::Json {
                text: r#"{"id": 42}"#.into()
            }
        );
    }

    #[test]
    fn resolve_reports_unresolved_once_and_skips_disabled_rows() {
        let mut request = Request::new(Method::Get, "{{base}}/{{path}}")
            .with_header("X-Trace", "{{base}}")
            .with_body(Body::Multipart {
                parts: vec![
                    crate::MultipartPart::text("note", "{{note}}"),
                    crate::MultipartPart::file("upload", "{{dir}}/a.txt"),
                ],
            });
        request
            .headers
            .push(KeyValue::new("X-Off", "{{ignored}}").disabled());

        let resolved = request.resolve(&Variables::new().with_layer([("dir", "/tmp")]));
        assert_eq!(resolved.unresolved, ["base", "path", "note"]);
        assert_eq!(resolved.request.headers[1].value, "{{ignored}}");
        let Body::Multipart { parts } = &resolved.request.body else {
            unreachable!()
        };
        assert_eq!(parts[1].value, PartValue::File("/tmp/a.txt".into()));
    }

    #[test]
    fn resolve_substitutes_auth_fields() {
        let request = Request::new(Method::Get, "/").with_auth(Auth::Basic {
            username: "{{user}}".into(),
            password: "{{password}}".into(),
        });
        let resolved = request.resolve(&Variables::new().with_layer([("user", "admin")]));
        assert_eq!(resolved.unresolved, ["password"]);
        assert_eq!(
            resolved.request.auth,
            Auth::Basic {
                username: "admin".into(),
                password: "{{password}}".into(),
            }
        );
    }

    #[test]
    fn has_header_ignores_case_and_disabled_rows() {
        let mut request = Request::new(Method::Get, "/").with_header("Content-Type", "a");
        assert!(request.has_header("content-type"));
        request.headers[0].enabled = false;
        assert!(!request.has_header("content-type"));
    }
}
