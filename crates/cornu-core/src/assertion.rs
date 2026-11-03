use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Response;
use crate::capture::{CaptureError, query_body};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssertSource {
    #[default]
    Status,
    Header,
    Body,
    /// In milliseconds.
    Duration,
    /// Body size in bytes.
    Size,
}

impl AssertSource {
    pub const ALL: [AssertSource; 5] = [
        AssertSource::Status,
        AssertSource::Header,
        AssertSource::Body,
        AssertSource::Duration,
        AssertSource::Size,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AssertSource::Status => "Status",
            AssertSource::Header => "Header",
            AssertSource::Body => "Body",
            AssertSource::Duration => "Time (ms)",
            AssertSource::Size => "Size (bytes)",
        }
    }

    pub fn takes_expression(self) -> bool {
        matches!(self, AssertSource::Header | AssertSource::Body)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    #[default]
    Equals,
    NotEquals,
    Contains,
    NotContains,
    LessThan,
    LessOrEqual,
    GreaterThan,
    GreaterOrEqual,
    Exists,
    NotExists,
}

impl Operator {
    pub const ALL: [Operator; 10] = [
        Operator::Equals,
        Operator::NotEquals,
        Operator::Contains,
        Operator::NotContains,
        Operator::LessThan,
        Operator::LessOrEqual,
        Operator::GreaterThan,
        Operator::GreaterOrEqual,
        Operator::Exists,
        Operator::NotExists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Operator::Equals => "equals",
            Operator::NotEquals => "not equals",
            Operator::Contains => "contains",
            Operator::NotContains => "not contains",
            Operator::LessThan => "<",
            Operator::LessOrEqual => "<=",
            Operator::GreaterThan => ">",
            Operator::GreaterOrEqual => ">=",
            Operator::Exists => "exists",
            Operator::NotExists => "not exists",
        }
    }

    pub fn takes_value(self) -> bool {
        !matches!(self, Operator::Exists | Operator::NotExists)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    #[serde(default)]
    pub source: AssertSource,
    /// A JSONPath for the body (empty takes the whole body), a header name for a header.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expression: String,
    #[serde(default)]
    pub operator: Operator,
    /// May contain `{{variables}}`, resolved with the request.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expected: String,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

impl Assertion {
    pub fn new(
        source: AssertSource,
        expression: impl Into<String>,
        operator: Operator,
        expected: impl Into<String>,
    ) -> Self {
        Self {
            source,
            expression: expression.into(),
            operator,
            expected: expected.into(),
            enabled: true,
        }
    }

    pub fn status(expected: u16) -> Self {
        Self::new(
            AssertSource::Status,
            "",
            Operator::Equals,
            expected.to_string(),
        )
    }

    pub fn body(path: impl Into<String>, operator: Operator, expected: impl Into<String>) -> Self {
        Self::new(AssertSource::Body, path, operator, expected)
    }

    fn actual(&self, response: &Response) -> Result<String, CaptureError> {
        let expression = self.expression.trim();
        match self.source {
            AssertSource::Status => Ok(response.status.to_string()),
            AssertSource::Duration => Ok(response.elapsed.as_millis().to_string()),
            AssertSource::Size => Ok(response.size().to_string()),
            AssertSource::Header => response
                .header(expression)
                .map(str::to_owned)
                .ok_or_else(|| CaptureError::MissingHeader(expression.to_owned())),
            AssertSource::Body if expression.is_empty() => Ok(response.text().into_owned()),
            AssertSource::Body => query_body(expression, response),
        }
    }

    pub fn check(&self, response: &Response) -> AssertionResult {
        let actual = self.actual(response);
        let missing = matches!(
            actual,
            Err(CaptureError::NoMatch(_) | CaptureError::MissingHeader(_))
        );
        let outcome = match (self.operator, &actual) {
            (Operator::Exists, _) if missing => Err("missing".to_owned()),
            (Operator::NotExists, _) if missing => Ok(()),
            (_, Err(error)) => Err(error.to_string()),
            (Operator::Exists, Ok(_)) => Ok(()),
            (Operator::NotExists, Ok(actual)) => Err(format!("found {actual}")),
            (operator, Ok(actual)) => compare(operator, actual, self.expected.trim()),
        };
        AssertionResult {
            description: self.to_string(),
            actual: actual.ok(),
            error: outcome.err(),
        }
    }
}

fn compare(operator: Operator, actual: &str, expected: &str) -> Result<(), String> {
    let passed = match operator {
        Operator::Equals => actual == expected,
        Operator::NotEquals => actual != expected,
        Operator::Contains => actual.contains(expected),
        Operator::NotContains => !actual.contains(expected),
        _ => {
            let (Ok(left), Ok(right)) = (actual.parse::<f64>(), expected.parse::<f64>()) else {
                return Err(format!(
                    "cannot compare {actual:?} with {expected:?} as numbers"
                ));
            };
            match operator {
                Operator::LessThan => left < right,
                Operator::LessOrEqual => left <= right,
                Operator::GreaterThan => left > right,
                _ => left >= right,
            }
        }
    };
    if passed {
        Ok(())
    } else {
        Err(format!("got {actual}"))
    }
}

impl fmt::Display for Assertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let subject = match self.source {
            AssertSource::Status => "status",
            AssertSource::Duration => "time (ms)",
            AssertSource::Size => "size",
            AssertSource::Header | AssertSource::Body if !self.expression.trim().is_empty() => {
                self.expression.trim()
            }
            AssertSource::Header => "header",
            AssertSource::Body => "body",
        };
        write!(f, "{subject} {}", self.operator.label())?;
        if self.operator.takes_value() {
            write!(f, " {}", self.expected.trim())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionResult {
    pub description: String,
    pub actual: Option<String>,
    /// Why it failed; `None` when it passed.
    pub error: Option<String>,
}

impl AssertionResult {
    pub fn passed(&self) -> bool {
        self.error.is_none()
    }
}

pub fn check(assertions: &[Assertion], response: &Response) -> Vec<AssertionResult> {
    assertions
        .iter()
        .filter(|assertion| assertion.enabled)
        .map(|assertion| assertion.check(response))
        .collect()
}

/// With assertions, a response passes when they all do, whatever its status; without, when it is 2xx.
pub fn passed(response: &Response, results: &[AssertionResult]) -> bool {
    if results.is_empty() {
        response.is_success()
    } else {
        results.iter().all(AssertionResult::passed)
    }
}
