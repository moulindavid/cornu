use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::Response;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    #[default]
    Body,
    Header,
    Status,
}

impl CaptureSource {
    pub const ALL: [CaptureSource; 3] = [
        CaptureSource::Body,
        CaptureSource::Header,
        CaptureSource::Status,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CaptureSource::Body => "Body",
            CaptureSource::Header => "Header",
            CaptureSource::Status => "Status",
        }
    }
}

/// Stores a value of the response into a variable for the requests sent after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub variable: String,
    #[serde(default)]
    pub source: CaptureSource,
    /// A JSONPath for the body (empty takes the whole body), a header name for a header.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expression: String,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

impl Capture {
    pub fn body(variable: impl Into<String>, path: impl Into<String>) -> Self {
        Self::new(variable, CaptureSource::Body, path)
    }

    pub fn header(variable: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(variable, CaptureSource::Header, name)
    }

    pub fn status(variable: impl Into<String>) -> Self {
        Self::new(variable, CaptureSource::Status, "")
    }

    fn new(
        variable: impl Into<String>,
        source: CaptureSource,
        expression: impl Into<String>,
    ) -> Self {
        Self {
            variable: variable.into(),
            source,
            expression: expression.into(),
            enabled: true,
        }
    }

    /// `{{token}}` is accepted for `token`.
    pub fn name(&self) -> &str {
        let name = self.variable.trim();
        name.strip_prefix("{{")
            .and_then(|inner| inner.strip_suffix("}}"))
            .map_or(name, str::trim)
    }

    pub fn extract(&self, response: &Response) -> Result<String, CaptureError> {
        let expression = self.expression.trim();
        match self.source {
            CaptureSource::Status => Ok(response.status.to_string()),
            CaptureSource::Header => response
                .header(expression)
                .map(str::to_owned)
                .ok_or_else(|| CaptureError::MissingHeader(expression.to_owned())),
            CaptureSource::Body if expression.is_empty() => Ok(response.text().into_owned()),
            CaptureSource::Body => {
                let path = JsonPath::parse(expression)
                    .map_err(|error| CaptureError::InvalidPath(error.to_string()))?;
                let body: Value =
                    serde_json::from_slice(&response.body).map_err(|_| CaptureError::NotJson)?;
                match path.query(&body).all().as_slice() {
                    [] => Err(CaptureError::NoMatch(expression.to_owned())),
                    [value] => Ok(text(value)),
                    values => {
                        Ok(Value::Array(values.iter().map(|&v| v.clone()).collect()).to_string())
                    }
                }
            }
        }
    }
}

/// Strings are taken without their quotes, anything else as JSON.
fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("invalid JSONPath: {0}")]
    InvalidPath(String),
    #[error("the body is not JSON")]
    NotJson,
    #[error("nothing matches {0}")]
    NoMatch(String),
    #[error("no {0} header")]
    MissingHeader(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub variable: String,
    pub value: Result<String, CaptureError>,
}

/// Disabled captures and captures without a variable name are skipped.
pub fn capture(captures: &[Capture], response: &Response) -> Vec<Captured> {
    captures
        .iter()
        .filter(|capture| capture.enabled && !capture.name().is_empty())
        .map(|capture| Captured {
            variable: capture.name().to_owned(),
            value: capture.extract(response),
        })
        .collect()
}

/// Sets each successful capture, replacing an earlier value of the same variable.
pub fn store(variables: &mut Vec<(String, String)>, captured: &[Captured]) {
    for Captured { variable, value } in captured {
        let Ok(value) = value else { continue };
        match variables.iter_mut().find(|(name, _)| name == variable) {
            Some((_, old)) => old.clone_from(value),
            None => variables.push((variable.clone(), value.clone())),
        }
    }
}
