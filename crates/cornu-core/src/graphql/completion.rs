//! Context-aware suggestions for a query being typed: fields, arguments, type conditions
//! and enum values. The query before the cursor is scanned, not parsed, so it may be
//! incomplete or invalid.

use super::{FullType, InputValue, Schema, TypeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte offset where the word being typed starts; it is replaced up to the cursor.
    pub start: usize,
    pub items: Vec<Suggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub kind: SuggestionKind,
    pub deprecated: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Field,
    Argument,
    Type,
    EnumValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a str),
    Punct(char),
    Spread,
    /// A string, number or `$variable`.
    Value,
}

/// `None` inside a string or comment, where nothing should pop up.
fn tokens(text: &str) -> Option<Vec<Token<'_>>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'#' => i += text[i..].find('\n')?,
            b'"' if text[i..].starts_with("\"\"\"") => {
                i += text[i + 3..].find("\"\"\"")? + 6;
                tokens.push(Token::Value);
            }
            b'"' => {
                i += 1;
                loop {
                    match bytes.get(i) {
                        None | Some(b'\n') => return None,
                        Some(b'\\') => i += 2,
                        Some(b'"') => break,
                        Some(_) => i += 1,
                    }
                }
                i += 1;
                tokens.push(Token::Value);
            }
            b'.' if text[i..].starts_with("...") => {
                i += 3;
                tokens.push(Token::Spread);
            }
            b'$' | b'-' | b'0'..=b'9' => {
                i += 1 + word_len(&text[i + 1..]);
                tokens.push(Token::Value);
            }
            _ if is_name_char(c) => {
                let len = word_len(&text[i..]);
                tokens.push(Token::Name(&text[i..i + len]));
                i += len;
            }
            _ if c.is_ascii_whitespace() || c == b',' => i += 1,
            _ if c.is_ascii() => {
                tokens.push(Token::Punct(c as char));
                i += 1;
            }
            _ => i += text[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    Some(tokens)
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Also covers the digits, dots and exponents of numbers.
fn word_len(text: &str) -> usize {
    text.bytes()
        .take_while(|&c| is_name_char(c) || c == b'.' || c == b'+' || c == b'-')
        .count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    AfterAlias,
    AfterSpread,
    AfterOn,
    AfterAt,
    AfterDirective,
}

#[derive(Debug, Clone, Copy)]
enum Target<'a> {
    Field(&'a str),
    Type(&'a str),
    SameType,
}

enum Frame<'s, 'a> {
    Selection {
        ty: Option<&'s FullType>,
        target: Option<Target<'a>>,
        mode: Mode,
    },
    Args {
        args: &'s [InputValue],
        used: Vec<&'a str>,
        current: Option<&'a str>,
        expect_value: bool,
    },
    /// Variable definitions, directive arguments, list and object values.
    Value,
}

#[derive(Default)]
struct Definition<'a> {
    operation: Option<&'a str>,
    after_on: bool,
    type_condition: Option<&'a str>,
}

struct Scanner<'s, 'a> {
    schema: &'s Schema,
    stack: Vec<Frame<'s, 'a>>,
    definition: Definition<'a>,
}

impl<'s, 'a> Scanner<'s, 'a> {
    fn feed(&mut self, token: Token<'a>) {
        match self.stack.last_mut() {
            None => self.top_level(token),
            Some(Frame::Value) => match token {
                Token::Punct('{' | '[' | '(') => self.stack.push(Frame::Value),
                Token::Punct('}' | ']' | ')') => self.pop(),
                _ => {}
            },
            Some(Frame::Args {
                used,
                current,
                expect_value,
                ..
            }) => match token {
                Token::Punct(')') => self.pop(),
                Token::Punct('{' | '[') => self.stack.push(Frame::Value),
                Token::Punct(':') => *expect_value = true,
                Token::Name(name) if !*expect_value => {
                    used.push(name);
                    *current = Some(name);
                }
                Token::Name(_) | Token::Value => *expect_value = false,
                _ => {}
            },
            Some(Frame::Selection { ty, target, mode }) => {
                let ty = *ty;
                match (token, *mode) {
                    (Token::Name("on"), Mode::AfterSpread) => *mode = Mode::AfterOn,
                    (Token::Name(_), Mode::AfterSpread) => {
                        *target = None;
                        *mode = Mode::Normal;
                    }
                    (Token::Name(name), Mode::AfterOn) => {
                        *target = Some(Target::Type(name));
                        *mode = Mode::Normal;
                    }
                    (Token::Name(_), Mode::AfterAt) => *mode = Mode::AfterDirective,
                    (Token::Name(name), _) => {
                        *target = Some(Target::Field(name));
                        *mode = Mode::Normal;
                    }
                    (Token::Spread, _) => {
                        *target = Some(Target::SameType);
                        *mode = Mode::AfterSpread;
                    }
                    (Token::Punct(':'), Mode::Normal) => *mode = Mode::AfterAlias,
                    (Token::Punct('@'), _) => *mode = Mode::AfterAt,
                    (Token::Punct('('), Mode::AfterDirective) => {
                        *mode = Mode::Normal;
                        self.stack.push(Frame::Value);
                    }
                    (Token::Punct('('), _) => {
                        let field = match target {
                            Some(Target::Field(name)) => ty.and_then(|ty| field(ty, name)),
                            _ => None,
                        };
                        self.stack.push(Frame::Args {
                            args: field.map_or(&[], |field| &field.args),
                            used: Vec::new(),
                            current: None,
                            expect_value: false,
                        });
                    }
                    (Token::Punct('{'), _) => {
                        let child = match target.take() {
                            Some(Target::Field(name)) => ty
                                .and_then(|ty| field(ty, name))
                                .and_then(|field| self.schema.type_named(field.ty.named())),
                            Some(Target::Type(name)) => self.schema.type_named(name),
                            Some(Target::SameType) => ty,
                            None => None,
                        };
                        *mode = Mode::Normal;
                        self.stack.push(Frame::Selection {
                            ty: child,
                            target: None,
                            mode: Mode::Normal,
                        });
                    }
                    (Token::Punct('}'), _) => self.pop(),
                    _ => {}
                }
            }
        }
    }

    fn top_level(&mut self, token: Token<'a>) {
        let definition = &mut self.definition;
        match token {
            Token::Name("on") => definition.after_on = true,
            Token::Name(name) if definition.after_on => {
                definition.type_condition = Some(name);
                definition.after_on = false;
            }
            Token::Name(name) if definition.operation.is_none() => {
                definition.operation = Some(name);
            }
            Token::Punct('(') => self.stack.push(Frame::Value),
            Token::Punct('{') => {
                let definition = std::mem::take(&mut self.definition);
                let ty = match (definition.operation, definition.type_condition) {
                    (Some("fragment"), Some(name)) => Some(name),
                    (Some("fragment"), None) => None,
                    (operation, _) => {
                        let operation = operation.unwrap_or("query");
                        self.schema
                            .roots()
                            .find(|(root, _)| *root == operation)
                            .map(|(_, name)| name)
                    }
                };
                self.stack.push(Frame::Selection {
                    ty: ty.and_then(|name| self.schema.type_named(name)),
                    target: None,
                    mode: Mode::Normal,
                });
            }
            _ => {}
        }
    }

    fn pop(&mut self) {
        self.stack.pop();
        if let Some(Frame::Args { expect_value, .. }) = self.stack.last_mut() {
            *expect_value = false;
        }
    }

    fn suggestions(&self) -> Vec<Suggestion> {
        match self.stack.last() {
            Some(Frame::Selection {
                ty: Some(ty), mode, ..
            }) => match mode {
                Mode::AfterOn => self.type_conditions(),
                Mode::Normal | Mode::AfterAlias => fields(ty),
                _ => Vec::new(),
            },
            Some(Frame::Args {
                args,
                used,
                current,
                expect_value,
            }) => {
                if !*expect_value {
                    return args
                        .iter()
                        .filter(|arg| !used.contains(&arg.name.as_str()))
                        .map(argument)
                        .collect();
                }
                let arg = args.iter().find(|arg| Some(arg.name.as_str()) == *current);
                arg.and_then(|arg| self.schema.type_named(arg.ty.named()))
                    .map(enum_values)
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    fn type_conditions(&self) -> Vec<Suggestion> {
        self.schema
            .types
            .iter()
            .filter(|ty| {
                !ty.is_builtin()
                    && matches!(
                        ty.kind,
                        TypeKind::Object | TypeKind::Interface | TypeKind::Union
                    )
            })
            .map(|ty| Suggestion {
                label: ty.name.clone(),
                insert: ty.name.clone(),
                detail: ty.kind.label().to_owned(),
                kind: SuggestionKind::Type,
                deprecated: false,
                description: ty.description.clone(),
            })
            .collect()
    }
}

fn field<'s>(ty: &'s FullType, name: &str) -> Option<&'s super::Field> {
    ty.fields.iter().find(|field| field.name == name)
}

fn fields(ty: &FullType) -> Vec<Suggestion> {
    let mut items: Vec<Suggestion> = ty
        .fields
        .iter()
        .map(|field| Suggestion {
            label: field.name.clone(),
            insert: field.name.clone(),
            detail: field.ty.to_string(),
            kind: SuggestionKind::Field,
            deprecated: field.is_deprecated,
            description: field.description.clone(),
        })
        .collect();
    if matches!(
        ty.kind,
        TypeKind::Object | TypeKind::Interface | TypeKind::Union
    ) {
        items.push(Suggestion {
            label: "__typename".into(),
            insert: "__typename".into(),
            detail: "String!".into(),
            kind: SuggestionKind::Field,
            deprecated: false,
            description: None,
        });
    }
    items
}

fn argument(arg: &InputValue) -> Suggestion {
    Suggestion {
        label: arg.name.clone(),
        insert: format!("{}: ", arg.name),
        detail: arg.ty.to_string(),
        kind: SuggestionKind::Argument,
        deprecated: false,
        description: arg.description.clone(),
    }
}

fn enum_values(ty: &FullType) -> Vec<Suggestion> {
    ty.enum_values
        .iter()
        .map(|value| Suggestion {
            label: value.name.clone(),
            insert: value.name.clone(),
            detail: ty.name.clone(),
            kind: SuggestionKind::EnumValue,
            deprecated: value.is_deprecated,
            description: value.description.clone(),
        })
        .collect()
}

/// What can be typed at byte offset `cursor` of `query`; prefix matches come before
/// other matches of the word under the cursor.
pub fn complete(schema: &Schema, query: &str, cursor: usize) -> Completion {
    let cursor = cursor.min(query.len());
    let before = &query[..cursor];
    let word = before
        .bytes()
        .rev()
        .take_while(|&c| is_name_char(c))
        .count();
    let start = cursor - word;
    let partial = before[start..].to_lowercase();
    let empty = Completion {
        start,
        items: Vec::new(),
    };
    if partial.starts_with(|c: char| c.is_ascii_digit()) {
        return empty;
    }
    if before[..start].ends_with('$') {
        return empty;
    }
    let Some(tokens) = tokens(&query[..start]) else {
        return empty;
    };

    let mut scanner = Scanner {
        schema,
        stack: Vec::new(),
        definition: Definition::default(),
    };
    for token in tokens {
        scanner.feed(token);
    }
    let (mut items, others): (Vec<_>, Vec<_>) = scanner
        .suggestions()
        .into_iter()
        .filter(|item| item.label.to_lowercase().contains(&partial))
        .partition(|item| item.label.to_lowercase().starts_with(&partial));
    items.extend(others);
    if items.iter().any(|item| item.label == before[start..]) {
        items.clear();
    }
    Completion { start, items }
}
