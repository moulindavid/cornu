use std::collections::HashMap;
use std::ops::Range;

use time::OffsetDateTime;
use time::macros::format_description;

const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    Variable { name: &'a str, span: Range<usize> },
}

pub fn segments(template: &str) -> Vec<Segment<'_>> {
    let bytes = template.as_bytes();
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(len) = template[i + 2..].find("}}")
        {
            let inner = &template[i + 2..i + 2 + len];
            let name = inner.trim();
            if !name.is_empty() && !inner.contains(['{', '}', '\n']) {
                if text_start < i {
                    out.push(Segment::Text(&template[text_start..i]));
                }
                let end = i + 2 + len + 2;
                out.push(Segment::Variable { name, span: i..end });
                i = end;
                text_start = end;
                continue;
            }
        }
        i += 1;
    }
    if text_start < template.len() {
        out.push(Segment::Text(&template[text_start..]));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpolated {
    pub text: String,
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variables {
    layers: Vec<HashMap<String, String>>,
}

impl Variables {
    pub fn new() -> Self {
        Self::default()
    }

    /// Variables of a later layer shadow those of earlier layers.
    pub fn push_layer<I, K, V>(&mut self, variables: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.layers.push(
            variables
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        );
    }

    pub fn with_layer<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.push_layer(variables);
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.layers
            .iter()
            .rev()
            .find_map(|layer| layer.get(name))
            .map(String::as_str)
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.get(name).is_some() || is_dynamic(name)
    }

    /// Unresolved placeholders are kept verbatim in the output.
    pub fn interpolate(&self, template: &str) -> Interpolated {
        let mut result = Interpolated {
            text: String::with_capacity(template.len()),
            unresolved: Vec::new(),
        };
        self.expand(template, 0, &mut result);
        result
    }

    fn expand(&self, template: &str, depth: usize, out: &mut Interpolated) {
        for segment in segments(template) {
            match segment {
                Segment::Text(text) => out.text.push_str(text),
                Segment::Variable { name, span } => {
                    let generated = match self.get(name) {
                        Some(value) if depth < MAX_DEPTH => {
                            self.expand(value, depth + 1, out);
                            continue;
                        }
                        Some(_) => None,
                        None => dynamic(name),
                    };
                    match generated {
                        Some(value) => out.text.push_str(&value),
                        None => {
                            out.text.push_str(&template[span]);
                            if !out.unresolved.iter().any(|n| n == name) {
                                out.unresolved.push(name.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
}

pub const DYNAMIC_VARIABLES: [&str; 6] = [
    "$uuid",
    "$guid",
    "$randomUUID",
    "$timestamp",
    "$isoTimestamp",
    "$randomInt",
];

pub fn is_dynamic(name: &str) -> bool {
    DYNAMIC_VARIABLES.contains(&name)
}

fn dynamic(name: &str) -> Option<String> {
    let value = match name {
        "$uuid" | "$guid" | "$randomUUID" => uuid::Uuid::new_v4().to_string(),
        "$timestamp" => OffsetDateTime::now_utc().unix_timestamp().to_string(),
        "$isoTimestamp" => OffsetDateTime::now_utc()
            .format(format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
            ))
            .ok()?,
        "$randomInt" => fastrand::u32(0..=1000).to_string(),
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        Variables::new().with_layer(pairs.iter().copied())
    }

    #[test]
    fn segments_split_text_and_variables() {
        assert_eq!(
            segments("{{base}}/users/{{ id }}?x=1"),
            [
                Segment::Variable {
                    name: "base",
                    span: 0..8
                },
                Segment::Text("/users/"),
                Segment::Variable {
                    name: "id",
                    span: 15..23
                },
                Segment::Text("?x=1"),
            ]
        );
    }

    #[test]
    fn segments_ignore_malformed_placeholders() {
        assert_eq!(
            segments("{{}} {{ }} {{open"),
            [Segment::Text("{{}} {{ }} {{open")]
        );
        assert_eq!(segments("{{a\nb}}"), [Segment::Text("{{a\nb}}")]);
    }

    #[test]
    fn segments_handle_surrounding_json_braces() {
        let template = r#"{"a":{{value}}}"#;
        let names: Vec<_> = segments(template)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Variable { name, .. } => Some(name),
                Segment::Text(_) => None,
            })
            .collect();
        assert_eq!(names, ["value"]);
        assert_eq!(
            vars(&[("value", "1")]).interpolate(template).text,
            r#"{"a":1}"#
        );
    }

    #[test]
    fn segments_keep_multibyte_text_intact() {
        let result = vars(&[("v", "é")]).interpolate("café {{v}} ✓");
        assert_eq!(result.text, "café é ✓");
    }

    #[test]
    fn later_layers_shadow_earlier_ones() {
        let variables = Variables::new()
            .with_layer([("host", "global.example"), ("port", "80")])
            .with_layer([("host", "dev.example")]);
        assert_eq!(
            variables.interpolate("{{host}}:{{port}}").text,
            "dev.example:80"
        );
    }

    #[test]
    fn unresolved_are_kept_and_reported_once() {
        let result = vars(&[("a", "1")]).interpolate("{{a}} {{missing}} {{ missing }} {{other}}");
        assert_eq!(result.text, "1 {{missing}} {{ missing }} {{other}}");
        assert_eq!(result.unresolved, ["missing", "other"]);
    }

    #[test]
    fn values_are_expanded_recursively() {
        let variables = vars(&[
            ("base_url", "{{scheme}}://{{host}}"),
            ("scheme", "https"),
            ("host", "api.example"),
        ]);
        assert_eq!(
            variables.interpolate("{{base_url}}/v1").text,
            "https://api.example/v1"
        );
    }

    #[test]
    fn cycles_stop_and_are_reported() {
        let result = vars(&[("a", "{{b}}"), ("b", "{{a}}")]).interpolate("{{a}}");
        assert!(result.text.starts_with("{{"));
        assert!(!result.unresolved.is_empty());
    }

    #[test]
    fn dynamic_variables_are_generated_per_use() {
        let result = Variables::new().interpolate("{{$uuid}} {{$uuid}}");
        let ids: Vec<_> = result.text.split(' ').collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()));
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn dynamic_formats() {
        let variables = Variables::new();
        let timestamp: i64 = variables
            .interpolate("{{$timestamp}}")
            .text
            .parse()
            .unwrap();
        assert!(timestamp > 1_700_000_000);
        let iso = variables.interpolate("{{$isoTimestamp}}").text;
        assert_eq!(iso.len(), "2026-10-21T12:00:00.000Z".len(), "{iso}");
        assert!(iso.ends_with('Z'));
        let random: u32 = variables
            .interpolate("{{$randomInt}}")
            .text
            .parse()
            .unwrap();
        assert!(random <= 1000);
    }

    #[test]
    fn every_dynamic_variable_generates_a_value() {
        for name in DYNAMIC_VARIABLES {
            assert!(dynamic(name).is_some(), "{name}");
        }
    }

    #[test]
    fn is_defined_covers_layers_and_dynamic_names() {
        let variables = vars(&[("host", "x")]);
        assert!(variables.is_defined("host"));
        assert!(variables.is_defined("$uuid"));
        assert!(!variables.is_defined("missing"));
        assert!(!variables.is_defined("$unknown"));
    }

    #[test]
    fn user_variables_override_dynamic_ones() {
        assert_eq!(
            vars(&[("$uuid", "fixed")]).interpolate("{{$uuid}}").text,
            "fixed"
        );
    }
}
