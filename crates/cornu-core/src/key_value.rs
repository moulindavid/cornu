use serde::{Deserialize, Serialize};

/// A key/value row, as used for headers, query parameters and form fields.
///
/// Disabled rows are kept (so the user can toggle them back on) but are not sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    #[serde(default = "enabled_default", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
            description: None,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// Iterates over the enabled rows as `(key, value)` pairs.
pub fn enabled(rows: &[KeyValue]) -> impl Iterator<Item = (&str, &str)> {
    rows.iter()
        .filter(|row| row.enabled)
        .map(|row| (row.key.as_str(), row.value.as_str()))
}

pub(crate) fn enabled_default() -> bool {
    true
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_skips_disabled_rows() {
        let rows = [
            KeyValue::new("a", "1"),
            KeyValue::new("b", "2").disabled(),
            KeyValue::new("c", "3"),
        ];
        let pairs: Vec<_> = enabled(&rows).collect();
        assert_eq!(pairs, [("a", "1"), ("c", "3")]);
    }

    #[test]
    fn enabled_flag_is_omitted_when_true() {
        let json = serde_json::to_string(&KeyValue::new("a", "1")).unwrap();
        assert_eq!(json, r#"{"key":"a","value":"1"}"#);
        let row: KeyValue = serde_json::from_str(&json).unwrap();
        assert!(row.enabled);
    }
}
