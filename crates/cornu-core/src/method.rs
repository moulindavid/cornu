use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An HTTP request method.
///
/// Standard methods are matched case-insensitively. Any other valid token
/// (for example `PURGE` or `PROPFIND`) is kept verbatim as [`Method::Custom`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
    Trace,
    Connect,
    Custom(String),
}

impl Method {
    /// The standard methods, in the order they are usually shown in a UI.
    pub const STANDARD: [Method; 9] = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
        Method::Head,
        Method::Options,
        Method::Trace,
        Method::Connect,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Connect => "CONNECT",
            Method::Custom(name) => name,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid HTTP method {0:?}")]
pub struct InvalidMethod(pub String);

impl FromStr for Method {
    type Err = InvalidMethod;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(standard) = Method::STANDARD
            .iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s))
        {
            return Ok(standard.clone());
        }
        if !s.is_empty() && s.bytes().all(is_token_char) {
            Ok(Method::Custom(s.to_owned()))
        } else {
            Err(InvalidMethod(s.to_owned()))
        }
    }
}

impl TryFrom<String> for Method {
    type Error = InvalidMethod;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Method> for String {
    fn from(method: Method) -> Self {
        method.as_str().to_owned()
    }
}

/// `tchar` from RFC 9110, section 5.6.2.
fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_methods_case_insensitively() {
        assert_eq!("get".parse(), Ok(Method::Get));
        assert_eq!("Patch".parse(), Ok(Method::Patch));
        assert_eq!("DELETE".parse(), Ok(Method::Delete));
    }

    #[test]
    fn keeps_custom_methods_verbatim() {
        assert_eq!("PURGE".parse(), Ok(Method::Custom("PURGE".into())));
        assert_eq!(Method::Custom("PROPFIND".into()).to_string(), "PROPFIND");
    }

    #[test]
    fn rejects_invalid_tokens() {
        assert!("".parse::<Method>().is_err());
        assert!("GET POST".parse::<Method>().is_err());
        assert!("GÉT".parse::<Method>().is_err());
    }

    #[test]
    fn serializes_as_plain_string() {
        assert_eq!(serde_json::to_string(&Method::Post).unwrap(), r#""POST""#);
        let custom: Method = serde_json::from_str(r#""LINK""#).unwrap();
        assert_eq!(custom, Method::Custom("LINK".into()));
        assert!(serde_json::from_str::<Method>(r#""BAD METHOD""#).is_err());
    }
}
