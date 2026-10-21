use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::KeyValue;
use crate::key_value::{enabled_default, is_true};

/// The body of a request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    #[default]
    None,
    /// Free text sent as-is, with a user-chosen content type.
    Raw {
        text: String,
        #[serde(default = "default_raw_content_type")]
        content_type: String,
    },
    /// JSON text, sent with `application/json`.
    Json { text: String },
    /// `application/x-www-form-urlencoded` fields.
    FormUrlEncoded {
        #[serde(default)]
        fields: Vec<KeyValue>,
    },
    /// `multipart/form-data` parts.
    Multipart {
        #[serde(default)]
        parts: Vec<MultipartPart>,
    },
    /// The content of a file, sent as-is.
    Binary {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
    },
}

impl Body {
    /// The `Content-Type` this body implies, if any.
    ///
    /// For multipart bodies the boundary is chosen at send time, so only the
    /// media type is returned.
    pub fn content_type(&self) -> Option<&str> {
        match self {
            Body::None => None,
            Body::Raw { content_type, .. } => Some(content_type),
            Body::Json { .. } => Some("application/json"),
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
}

fn default_raw_content_type() -> String {
    "text/plain".to_owned()
}

/// One part of a multipart body.
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
