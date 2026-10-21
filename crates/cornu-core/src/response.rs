use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Headers in the order they were received; names may repeat.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub elapsed: Duration,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn reason(&self) -> Option<&'static str> {
        reqwest::StatusCode::from_u16(self.status)
            .ok()?
            .canonical_reason()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    pub fn size(&self) -> usize {
        self.body.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16) -> Response {
        Response {
            status,
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("Set-Cookie".into(), "a=1".into()),
                ("Set-Cookie".into(), "b=2".into()),
            ],
            body: br#"{"ok":true}"#.to_vec(),
            elapsed: Duration::from_millis(12),
        }
    }

    #[test]
    fn header_lookup_is_case_insensitive_and_returns_first() {
        let response = response(200);
        assert_eq!(response.content_type(), Some("application/json"));
        assert_eq!(response.header("set-cookie"), Some("a=1"));
        assert_eq!(response.header("missing"), None);
    }

    #[test]
    fn success_covers_2xx_only() {
        assert!(response(204).is_success());
        assert!(!response(301).is_success());
        assert!(!response(404).is_success());
    }

    #[test]
    fn reason_is_the_canonical_phrase() {
        assert_eq!(response(404).reason(), Some("Not Found"));
        assert_eq!(response(799).reason(), None);
    }

    #[test]
    fn text_and_size() {
        let response = response(200);
        assert_eq!(response.text(), r#"{"ok":true}"#);
        assert_eq!(response.size(), 11);
    }
}
