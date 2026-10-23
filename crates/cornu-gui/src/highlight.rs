use std::ops::Range;

use eframe::egui::{Color32, FontId, TextFormat, Visuals, text::LayoutJob};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Key,
    String,
    Number,
    Literal,
    Punctuation,
}

pub fn json_tokens(text: &str) -> Vec<(Range<usize>, Kind)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                let next = bytes[i..].iter().find(|b| !b.is_ascii_whitespace());
                let kind = if next == Some(&b':') {
                    Kind::Key
                } else {
                    Kind::String
                };
                tokens.push((start..i, kind));
            }
            b'-' | b'0'..=b'9' => {
                while i < bytes.len()
                    && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                tokens.push((start..i, Kind::Number));
            }
            b't' | b'f' | b'n' => {
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                tokens.push((start..i, Kind::Literal));
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                i += 1;
                tokens.push((start..i, Kind::Punctuation));
            }
            _ => i += 1,
        }
    }
    tokens
}

/// ASCII-only case folding keeps byte offsets identical to the original text.
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let haystack = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    haystack
        .match_indices(&needle)
        .map(|(start, found)| start..start + found.len())
        .collect()
}

pub struct Palette {
    plain: Color32,
    key: Color32,
    string: Color32,
    number: Color32,
    literal: Color32,
    punctuation: Color32,
    highlight: Color32,
}

impl Palette {
    pub fn new(visuals: &Visuals) -> Self {
        if visuals.dark_mode {
            Self {
                plain: visuals.text_color(),
                key: Color32::from_rgb(0x7d, 0xc4, 0xe4),
                string: Color32::from_rgb(0xa6, 0xda, 0x95),
                number: Color32::from_rgb(0xf5, 0xa9, 0x7f),
                literal: Color32::from_rgb(0xc6, 0xa0, 0xf6),
                punctuation: visuals.weak_text_color(),
                highlight: Color32::from_rgba_unmultiplied(0xee, 0xd4, 0x9f, 0x70),
            }
        } else {
            Self {
                plain: visuals.text_color(),
                key: Color32::from_rgb(0x1e, 0x66, 0xf5),
                string: Color32::from_rgb(0x40, 0xa0, 0x2b),
                number: Color32::from_rgb(0xfe, 0x64, 0x0b),
                literal: Color32::from_rgb(0x88, 0x39, 0xef),
                punctuation: visuals.weak_text_color(),
                highlight: Color32::from_rgba_unmultiplied(0xdf, 0x8e, 0x1d, 0x60),
            }
        }
    }

    fn color(&self, kind: Option<Kind>) -> Color32 {
        match kind {
            None => self.plain,
            Some(Kind::Key) => self.key,
            Some(Kind::String) => self.string,
            Some(Kind::Number) => self.number,
            Some(Kind::Literal) => self.literal,
            Some(Kind::Punctuation) => self.punctuation,
        }
    }
}

/// `tokens` and `matches` must each be sorted and non-overlapping.
pub fn layout(
    text: &str,
    tokens: &[(Range<usize>, Kind)],
    matches: &[Range<usize>],
    palette: &Palette,
    font: FontId,
) -> LayoutJob {
    let mut cuts = Vec::with_capacity(2 + 2 * (tokens.len() + matches.len()));
    cuts.extend([0, text.len()]);
    for (range, _) in tokens {
        cuts.extend([range.start, range.end]);
    }
    for range in matches {
        cuts.extend([range.start, range.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut job = LayoutJob::default();
    for window in cuts.windows(2) {
        let (start, end) = (window[0], window[1]);
        let kind = containing(tokens, start, |(range, _)| range).map(|(_, kind)| *kind);
        let highlighted = containing(matches, start, |range| range).is_some();
        let format = TextFormat {
            font_id: font.clone(),
            color: palette.color(kind),
            background: if highlighted {
                palette.highlight
            } else {
                Color32::TRANSPARENT
            },
            ..Default::default()
        };
        job.append(&text[start..end], 0.0, format);
    }
    job
}

fn containing<T>(items: &[T], offset: usize, range: impl Fn(&T) -> &Range<usize>) -> Option<&T> {
    let index = items.partition_point(|item| range(item).end <= offset);
    items.get(index).filter(|item| range(item).start <= offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, Kind)> {
        json_tokens(text)
            .into_iter()
            .map(|(range, kind)| (&text[range], kind))
            .collect()
    }

    #[test]
    fn tokenizes_keys_values_and_punctuation() {
        assert_eq!(
            kinds(r#"{"id": -1.5e3, "ok": true, "name": "a\"b", "x": null}"#),
            [
                ("{", Kind::Punctuation),
                (r#""id""#, Kind::Key),
                (":", Kind::Punctuation),
                ("-1.5e3", Kind::Number),
                (",", Kind::Punctuation),
                (r#""ok""#, Kind::Key),
                (":", Kind::Punctuation),
                ("true", Kind::Literal),
                (",", Kind::Punctuation),
                (r#""name""#, Kind::Key),
                (":", Kind::Punctuation),
                (r#""a\"b""#, Kind::String),
                (",", Kind::Punctuation),
                (r#""x""#, Kind::Key),
                (":", Kind::Punctuation),
                ("null", Kind::Literal),
                ("}", Kind::Punctuation),
            ]
        );
    }

    #[test]
    fn unterminated_string_does_not_panic() {
        assert_eq!(
            kinds(r#"["abc"#),
            [("[", Kind::Punctuation), (r#""abc"#, Kind::String)]
        );
        assert_eq!(kinds(r#""\"#), [(r#""\"#, Kind::String)]);
    }

    #[test]
    fn matches_are_case_insensitive_and_keep_offsets() {
        let text = "Café USER user";
        let matches = find_matches(text, "user");
        assert_eq!(matches, [6..10, 11..15]);
        assert_eq!(&text[matches[0].clone()], "USER");
        assert!(find_matches(text, "").is_empty());
    }

    #[test]
    fn layout_covers_the_whole_text() {
        let text = r#"{"name": "cornu"}"#;
        let tokens = json_tokens(text);
        let matches = find_matches(text, "orn");
        let job = layout(
            text,
            &tokens,
            &matches,
            &Palette::new(&Visuals::dark()),
            FontId::monospace(12.0),
        );
        assert_eq!(job.text, text);
        let highlighted: String = job
            .sections
            .iter()
            .filter(|section| section.format.background != Color32::TRANSPARENT)
            .map(|section| &text[section.byte_range.start.0..section.byte_range.end.0])
            .collect();
        assert_eq!(highlighted, "orn");
    }
}
