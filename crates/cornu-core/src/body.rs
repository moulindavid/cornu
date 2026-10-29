use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::KeyValue;
use crate::key_value::{enabled_default, is_true};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    #[default]
    None,
    Raw {
        text: String,
        #[serde(default = "default_raw_content_type")]
        content_type: String,
    },
    Json {
        text: String,
    },
    FormUrlEncoded {
        #[serde(default)]
        fields: Vec<KeyValue>,
    },
    Multipart {
        #[serde(default)]
        parts: Vec<MultipartPart>,
    },
    Binary {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
    #[serde(rename = "graphql")]
    GraphQl {
        query: String,
        /// JSON text, kept as typed so `{{var}}` placeholders survive until send time.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        variables: String,
    },
}

impl Body {
    /// The multipart boundary is only known at send time, so it is not included.
    pub fn content_type(&self) -> Option<&str> {
        match self {
            Body::None => None,
            Body::Raw { content_type, .. } => Some(content_type),
            Body::Json { .. } | Body::GraphQl { .. } => Some("application/json"),
            Body::FormUrlEncoded { .. } => Some("application/x-www-form-urlencoded"),
            Body::Multipart { .. } => Some("multipart/form-data"),
            Body::Binary { content_type, .. } => Some(
                content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
            ),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Body::None)
    }

    /// The JSON document a GraphQL server expects; blank variables are left out.
    pub fn graphql_payload(query: &str, variables: &str) -> Result<String, serde_json::Error> {
        let mut payload = serde_json::Map::new();
        payload.insert("query".into(), query.into());
        if !variables.trim().is_empty() {
            payload.insert("variables".into(), serde_json::from_str(variables)?);
        }
        serde_json::to_string(&payload)
    }

    /// Recognizes `{"query": …, "variables": …}`, as sent by GraphQL clients.
    pub fn graphql_from_payload(text: &str) -> Option<Body> {
        let serde_json::Value::Object(payload) = serde_json::from_str(text).ok()? else {
            return None;
        };
        let query = payload.get("query")?.as_str()?.to_owned();
        let known = ["query", "variables", "operationName"];
        if payload.keys().any(|key| !known.contains(&key.as_str())) {
            return None;
        }
        let variables = match payload.get("variables") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(variables) => serde_json::to_string_pretty(variables).ok()?,
        };
        Some(Body::GraphQl { query, variables })
    }
}

fn default_raw_content_type() -> String {
    "text/plain".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartPart {
    pub name: String,
    #[serde(flatten)]
    pub value: PartValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default = "enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

impl MultipartPart {
    pub fn text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(name, PartValue::Text(text.into()))
    }

    pub fn file(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::new(name, PartValue::File(path.into()))
    }

    fn new(name: impl Into<String>, value: PartValue) -> Self {
        Self {
            name: name.into(),
            value,
            content_type: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartValue {
    Text(String),
    File(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_follows_body_kind() {
        assert_eq!(Body::None.content_type(), None);
        let json = Body::Json { text: "{}".into() };
        assert_eq!(json.content_type(), Some("application/json"));
        let binary = Body::Binary {
            path: "a.bin".into(),
            content_type: None,
        };
        assert_eq!(binary.content_type(), Some("application/octet-stream"));
    }

    #[test]
    fn graphql_payload_round_trips() {
        let query = "query ($code: ID!) { country(code: $code) { name } }";
        let variables = "{\n  \"code\": \"FR\"\n}";
        let payload = Body::graphql_payload(query, variables).unwrap();
        assert_eq!(
            payload,
            r#"{"query":"query ($code: ID!) { country(code: $code) { name } }","variables":{"code":"FR"}}"#
        );
        assert_eq!(
            Body::graphql_from_payload(&payload),
            Some(Body::GraphQl {
                query: query.into(),
                variables: variables.into()
            })
        );

        assert_eq!(
            Body::graphql_payload("{ a }", "  ").unwrap(),
            r#"{"query":"{ a }"}"#
        );
        assert!(Body::graphql_payload("{ a }", "{ broken").is_err());
        assert_eq!(
            Body::graphql_from_payload(r#"{"query": "{ a }", "extra": 1}"#),
            None
        );
        assert_eq!(Body::graphql_from_payload(r#"{"name": "x"}"#), None);
    }

    #[test]
    fn graphql_body_serializes_with_a_readable_tag() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            body: Body,
        }
        let original = Wrapper {
            body: Body::GraphQl {
                query: "{\n  countries { code }\n}".into(),
                variables: String::new(),
            },
        };
        let text = toml::to_string(&original).unwrap();
        assert!(text.contains("type = \"graphql\""), "{text}");
        assert!(!text.contains("variables"), "{text}");
        assert_eq!(toml::from_str::<Wrapper>(&text).unwrap(), original);
    }

    #[test]
    fn raw_defaults_to_text_plain() {
        let body: Body = toml::from_str("type = \"raw\"\ntext = \"hello\"").unwrap();
        assert_eq!(body.content_type(), Some("text/plain"));
    }

    #[test]
    fn multipart_round_trips_through_toml() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            body: Body,
        }
        let original = Wrapper {
            body: Body::Multipart {
                parts: vec![
                    MultipartPart::text("name", "cornu"),
                    MultipartPart::file("avatar", "./avatar.png"),
                ],
            },
        };
        let text = toml::to_string(&original).unwrap();
        assert!(text.contains("file = \"./avatar.png\""), "{text}");
        assert_eq!(toml::from_str::<Wrapper>(&text).unwrap(), original);
    }
}
