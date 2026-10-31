//! cURL command lines, as copied from browsers' devtools or API docs.

use std::path::PathBuf;
use std::str::FromStr;

use cornu_core::{Auth, Body, KeyValue, Method, MultipartPart, PartValue, Request};

use crate::ImportError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCurl {
    pub request: Request,
    pub warnings: Vec<String>,
}

pub fn is_curl(text: &str) -> bool {
    let mut words = text.split_whitespace();
    words.next().is_some_and(is_curl_program) && words.next().is_some()
}

fn is_curl_program(word: &str) -> bool {
    word.eq_ignore_ascii_case("curl") || word.eq_ignore_ascii_case("curl.exe")
}

pub fn parse(command: &str) -> Result<ParsedCurl, ImportError> {
    let tokens = tokenize(command)?;
    let mut args = tokens.into_iter();
    match args.next() {
        Some(first) if is_curl_program(&first) => {}
        _ => return Err(ImportError::Unsupported("not a curl command".into())),
    }
    let mut parser = Parser::default();
    while let Some(arg) = args.next() {
        parser.arg(arg, &mut args)?;
    }
    parser.finish()
}

#[derive(Clone, Copy)]
enum Opt {
    Method,
    Header,
    Data(DataKind),
    Json,
    Form {
        literal: bool,
    },
    User,
    Bearer,
    UserAgent,
    Referer,
    Cookie,
    Url,
    Get,
    Head,
    Silent,
    Insecure,
    /// Takes a value that does not affect the request; `warn` when the user may care.
    Skip {
        warn: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DataKind {
    Plain,
    Raw,
    Binary,
    UrlEncode,
}

fn long_option(name: &str) -> Option<Opt> {
    Some(match name {
        "request" => Opt::Method,
        "header" => Opt::Header,
        "data" | "data-ascii" => Opt::Data(DataKind::Plain),
        "data-raw" => Opt::Data(DataKind::Raw),
        "data-binary" => Opt::Data(DataKind::Binary),
        "data-urlencode" => Opt::Data(DataKind::UrlEncode),
        "json" => Opt::Json,
        "form" => Opt::Form { literal: false },
        "form-string" => Opt::Form { literal: true },
        "user" => Opt::User,
        "oauth2-bearer" => Opt::Bearer,
        "user-agent" => Opt::UserAgent,
        "referer" => Opt::Referer,
        "cookie" => Opt::Cookie,
        "url" => Opt::Url,
        "get" => Opt::Get,
        "head" => Opt::Head,
        "insecure" => Opt::Insecure,
        "compressed"
        | "location"
        | "silent"
        | "show-error"
        | "verbose"
        | "include"
        | "fail"
        | "globoff"
        | "no-buffer"
        | "path-as-is"
        | "http1.0"
        | "http1.1"
        | "http2"
        | "http2-prior-knowledge"
        | "http3"
        | "tlsv1.2"
        | "tlsv1.3"
        | "progress-bar"
        | "no-progress-meter"
        | "location-trusted" => Opt::Silent,
        "output" | "write-out" | "max-time" | "connect-timeout" | "max-redirs" | "retry"
        | "retry-delay" | "retry-max-time" | "dump-header" | "limit-rate" | "trace"
        | "trace-ascii" | "stderr" => Opt::Skip { warn: false },
        "proxy" | "proxy-user" | "cookie-jar" | "cert" | "key" | "cacert" | "capath"
        | "resolve" | "connect-to" | "range" | "upload-file" | "config" | "interface"
        | "ciphers" | "cert-type" | "key-type" | "pass" | "proxy-header" => {
            Opt::Skip { warn: true }
        }
        _ => return None,
    })
}

fn short_option(flag: char) -> Option<Opt> {
    Some(match flag {
        'X' => Opt::Method,
        'H' => Opt::Header,
        'd' => Opt::Data(DataKind::Plain),
        'F' => Opt::Form { literal: false },
        'u' => Opt::User,
        'A' => Opt::UserAgent,
        'e' => Opt::Referer,
        'b' => Opt::Cookie,
        'G' => Opt::Get,
        'I' => Opt::Head,
        'k' => Opt::Insecure,
        's' | 'S' | 'L' | 'v' | 'i' | 'f' | 'g' | 'N' | '#' | '0' | '1' | '2' | '3' => Opt::Silent,
        'o' | 'w' | 'm' | 'D' => Opt::Skip { warn: false },
        'x' | 'U' | 'c' | 'E' | 'r' | 'T' | 'K' => Opt::Skip { warn: true },
        _ => return None,
    })
}

impl Opt {
    fn takes_value(self) -> bool {
        !matches!(self, Opt::Get | Opt::Head | Opt::Silent | Opt::Insecure)
    }
}

#[derive(Default)]
struct Parser {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<KeyValue>,
    data: Vec<(DataKind, String)>,
    json: Vec<String>,
    parts: Vec<MultipartPart>,
    auth: Option<Auth>,
    get: bool,
    head: bool,
    warnings: Vec<String>,
}

impl Parser {
    fn arg(
        &mut self,
        arg: String,
        rest: &mut impl Iterator<Item = String>,
    ) -> Result<(), ImportError> {
        if let Some(name) = arg.strip_prefix("--") {
            if name.is_empty() {
                return Ok(());
            }
            let Some(opt) = long_option(name) else {
                self.warnings
                    .push(format!("unknown option --{name} ignored"));
                return Ok(());
            };
            let value = if opt.takes_value() {
                Some(value_for(&arg, rest)?)
            } else {
                None
            };
            self.apply(opt, &arg, value);
            return Ok(());
        }
        if let Some(flags) = arg.strip_prefix('-').filter(|flags| !flags.is_empty()) {
            for (index, flag) in flags.char_indices() {
                let name = format!("-{flag}");
                let Some(opt) = short_option(flag) else {
                    self.warnings.push(format!("unknown option {name} ignored"));
                    continue;
                };
                if opt.takes_value() {
                    let attached = &flags[index + flag.len_utf8()..];
                    let value = if attached.is_empty() {
                        value_for(&name, rest)?
                    } else {
                        attached.to_owned()
                    };
                    self.apply(opt, &name, Some(value));
                    break;
                }
                self.apply(opt, &name, None);
            }
            return Ok(());
        }
        self.set_url(arg);
        Ok(())
    }

    fn apply(&mut self, opt: Opt, name: &str, value: Option<String>) {
        let value = value.unwrap_or_default();
        match opt {
            Opt::Method => self.method = Some(value),
            Opt::Header => self.header(value),
            Opt::Data(kind) => self.data.push((kind, value)),
            Opt::Json => self.json.push(value),
            Opt::Form { literal } => self.form(value, literal),
            Opt::User => {
                let (username, password) = value.split_once(':').unwrap_or((&value, ""));
                self.auth = Some(Auth::Basic {
                    username: username.to_owned(),
                    password: password.to_owned(),
                });
            }
            Opt::Bearer => self.auth = Some(Auth::Bearer { token: value }),
            Opt::UserAgent => self.headers.push(KeyValue::new("User-Agent", value)),
            Opt::Referer => self.headers.push(KeyValue::new("Referer", value)),
            Opt::Cookie if value.contains('=') => {
                self.headers.push(KeyValue::new("Cookie", value));
            }
            Opt::Cookie => self.warnings.push(format!(
                "cookie file {value:?} ignored: only inline cookies are imported"
            )),
            Opt::Url => self.set_url(value),
            Opt::Get => self.get = true,
            Opt::Head => self.head = true,
            Opt::Insecure => self
                .warnings
                .push(format!("{name} ignored: TLS options are not supported yet")),
            Opt::Silent | Opt::Skip { warn: false } => {}
            Opt::Skip { warn: true } => {
                self.warnings.push(format!("option {name} ignored"));
            }
        }
    }

    fn set_url(&mut self, url: String) {
        if self.url.is_none() {
            self.url = Some(url);
        } else {
            self.warnings.push(format!(
                "extra URL {url} ignored: only the first one is imported"
            ));
        }
    }

    fn header(&mut self, header: String) {
        if header.starts_with('@') {
            self.warnings.push(format!("header file {header} ignored"));
            return;
        }
        // `-H 'Name;'` sends an empty header, `-H 'Name:'` removes a default one.
        let (key, value) = match header.split_once(':') {
            Some((key, value)) if !value.trim().is_empty() => (key.trim(), value.trim()),
            Some(_) => return,
            None => match header.strip_suffix(';') {
                Some(key) => (key.trim(), ""),
                None => return,
            },
        };
        if key.eq_ignore_ascii_case("authorization")
            && let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        {
            self.auth = Some(Auth::Bearer {
                token: token.trim().to_owned(),
            });
            return;
        }
        self.headers.push(KeyValue::new(key, value));
    }

    fn form(&mut self, field: String, literal: bool) {
        let Some((name, value)) = field.split_once('=') else {
            self.warnings.push(format!("form field {field:?} ignored"));
            return;
        };
        let mut part = MultipartPart::text(name, value);
        if !literal && let Some(file) = value.strip_prefix('@').or_else(|| value.strip_prefix('<'))
        {
            let mut attributes = file.split(';');
            let path = attributes.next().unwrap_or_default();
            part.value = PartValue::File(PathBuf::from(unquote(path)));
            for attribute in attributes {
                if let Some(content_type) = attribute.trim().strip_prefix("type=") {
                    part.content_type = Some(content_type.to_owned());
                }
            }
        }
        self.parts.push(part);
    }

    fn finish(mut self) -> Result<ParsedCurl, ImportError> {
        let Some(url) = self.url.take() else {
            return Err(ImportError::Unsupported(
                "the curl command has no URL".into(),
            ));
        };
        let (url, mut query) = split_url(&url);

        let mut body = Body::None;
        if self.get {
            let encoded = self.encoded_data();
            query.extend(decode_pairs(&encoded));
        } else if !self.parts.is_empty() {
            self.take_header("content-type");
            body = Body::Multipart {
                parts: std::mem::take(&mut self.parts),
            };
        } else if !self.json.is_empty() {
            body = Body::Json {
                text: self.json.concat(),
            };
            self.take_header("content-type");
        } else if !self.data.is_empty() {
            body = self.data_body();
        }

        if let Body::Json { text } = &body
            && let Some(graphql) = Body::graphql_from_payload(text)
        {
            body = graphql;
        }

        let method = match self.method.take() {
            Some(method) => Method::from_str(&method)
                .map_err(|error| ImportError::Unsupported(error.to_string()))?,
            None if self.head => Method::Head,
            None if !body.is_none() => Method::Post,
            None => Method::Get,
        };

        let mut request = Request::new(method, url);
        request.query = query;
        request.headers = self.headers;
        request.body = body;
        if let Some(auth) = self.auth {
            request.auth = auth;
        }
        Ok(ParsedCurl {
            request,
            warnings: self.warnings,
        })
    }

    fn data_body(&mut self) -> Body {
        let content_type = self.take_header("content-type");
        if let [(DataKind::Plain | DataKind::Binary, data)] = self.data.as_slice()
            && let Some(path) = data.strip_prefix('@')
        {
            return Body::Binary {
                path: PathBuf::from(path),
                content_type,
            };
        }
        let text = self.encoded_data();
        match content_type {
            Some(content_type) if content_type.to_ascii_lowercase().contains("json") => {
                if !content_type.eq_ignore_ascii_case("application/json") {
                    self.headers
                        .push(KeyValue::new("Content-Type", content_type));
                }
                Body::Json { text }
            }
            Some(content_type)
                if !content_type.eq_ignore_ascii_case("application/x-www-form-urlencoded") =>
            {
                Body::Raw { text, content_type }
            }
            _ if is_form(&text) => Body::FormUrlEncoded {
                fields: decode_pairs(&text),
            },
            _ => Body::Raw {
                text,
                content_type: "application/x-www-form-urlencoded".into(),
            },
        }
    }

    /// curl joins every `-d` with `&`, encoding only the `--data-urlencode` ones.
    fn encoded_data(&mut self) -> String {
        let mut pieces = Vec::new();
        for (kind, data) in self.data.drain(..) {
            pieces.push(match kind {
                DataKind::UrlEncode => match data.split_once('=') {
                    Some((name, content)) if !name.is_empty() => {
                        format!("{name}={}", encode(content))
                    }
                    Some((_, content)) => encode(content),
                    None => encode(&data),
                },
                DataKind::Plain if data.starts_with('@') => {
                    self.warnings
                        .push(format!("data file {data} sent as literal text"));
                    data
                }
                _ => data,
            });
        }
        pieces.join("&")
    }

    fn take_header(&mut self, name: &str) -> Option<String> {
        let index = self
            .headers
            .iter()
            .position(|header| header.key.eq_ignore_ascii_case(name))?;
        Some(self.headers.remove(index).value)
    }
}

fn value_for(name: &str, rest: &mut impl Iterator<Item = String>) -> Result<String, ImportError> {
    rest.next()
        .ok_or_else(|| ImportError::Unsupported(format!("option {name} needs a value")))
}

fn is_form(text: &str) -> bool {
    !text.is_empty() && text.split('&').all(|pair| pair.contains('='))
}

fn decode_pairs(text: &str) -> Vec<KeyValue> {
    url::form_urlencoded::parse(text.as_bytes())
        .map(|(key, value)| KeyValue::new(key, value))
        .collect()
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn split_url(raw: &str) -> (String, Vec<KeyValue>) {
    let raw = raw.split_once('#').map_or(raw, |(before, _)| before);
    match raw.split_once('?') {
        Some((base, query)) => (base.to_owned(), decode_pairs(query)),
        None => (raw.to_owned(), Vec::new()),
    }
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}

/// Splits like a POSIX shell (quotes, escapes, `$'…'`), plus cmd's `^` line continuations.
fn tokenize(input: &str) -> Result<Vec<String>, ImportError> {
    let mut tokens = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                tokens.extend(current.take());
            }
            '\\' | '^' if matches!(chars.peek(), Some('\n' | '\r')) => {
                while matches!(chars.peek(), Some('\n' | '\r')) {
                    chars.next();
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    current.get_or_insert_default().push(next);
                }
            }
            '\'' => {
                let token = current.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => token.push(c),
                        None => return Err(unterminated()),
                    }
                }
            }
            '"' => {
                let token = current.get_or_insert_default();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => token.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                token.push('\\');
                                token.push(c);
                            }
                            None => return Err(unterminated()),
                        },
                        Some(c) => token.push(c),
                        None => return Err(unterminated()),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                ansi_c_quoted(&mut chars, current.get_or_insert_default())?;
            }
            _ => current.get_or_insert_default().push(c),
        }
    }
    tokens.extend(current);
    Ok(tokens)
}

/// Chrome's "Copy as cURL" uses `$'…'` when a value contains quotes or control characters.
fn ansi_c_quoted(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    token: &mut String,
) -> Result<(), ImportError> {
    loop {
        match chars.next() {
            Some('\'') => return Ok(()),
            Some('\\') => {
                let escaped = match chars.next() {
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('e' | 'E') => '\u{1b}',
                    Some('0') => '\0',
                    Some(c @ ('x' | 'u' | 'U')) => {
                        let max = match c {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let mut hex = String::new();
                        while hex.len() < max
                            && let Some(digit) = chars.next_if(char::is_ascii_hexdigit)
                        {
                            hex.push(digit);
                        }
                        u32::from_str_radix(&hex, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .unwrap_or('\u{fffd}')
                    }
                    Some(c) => c,
                    None => return Err(unterminated()),
                };
                token.push(escaped);
            }
            Some(c) => token.push(c),
            None => return Err(unterminated()),
        }
    }
}

fn unterminated() -> ImportError {
    ImportError::Unsupported("unterminated quote in curl command".into())
}

/// Expects a request whose variables and inherited auth are already resolved.
pub fn to_curl(request: &Request) -> String {
    let mut first = vec!["curl".to_owned()];
    let implied = if request.body.is_none() {
        Method::Get
    } else {
        Method::Post
    };
    if request.method == Method::Head && request.body.is_none() {
        first.push("--head".into());
    } else if request.method != implied {
        first.extend(["-X".into(), quote(request.method.as_str())]);
    }
    first.push(quote(&full_url(request)));

    let mut options = Vec::new();
    let mut option = |flag: &str, value: &str| options.push(format!("{flag} {}", quote(value)));
    for (key, value) in cornu_core::key_value::enabled(&request.headers) {
        option("-H", &format!("{key}: {value}"));
    }
    match &request.auth {
        Auth::Basic { username, password } => option("-u", &format!("{username}:{password}")),
        auth => {
            if let Some((key, value)) = auth.header()
                && !request.has_header(&key)
            {
                option("-H", &format!("{key}: {value}"));
            }
        }
    }
    if let Body::Raw { .. } | Body::Json { .. } | Body::Binary { .. } | Body::GraphQl { .. } =
        request.body
        && let Some(content_type) = request.body.content_type()
        && !request.has_header("content-type")
    {
        option("-H", &format!("Content-Type: {content_type}"));
    }
    match &request.body {
        Body::None => {}
        Body::Raw { text, .. } | Body::Json { text } => option("--data-raw", text),
        Body::FormUrlEncoded { fields } => {
            for (key, value) in cornu_core::key_value::enabled(fields) {
                option("--data-urlencode", &format!("{key}={value}"));
            }
        }
        Body::Multipart { parts } => {
            for part in parts.iter().filter(|part| part.enabled) {
                match &part.value {
                    PartValue::Text(text) => {
                        option("--form-string", &format!("{}={text}", part.name));
                    }
                    PartValue::File(path) => {
                        let mut value = format!("{}=@\"{}\"", part.name, path.display());
                        if let Some(content_type) = &part.content_type {
                            value.push_str(&format!(";type={content_type}"));
                        }
                        option("-F", &value);
                    }
                }
            }
        }
        Body::Binary { path, .. } => option("--data-binary", &format!("@{}", path.display())),
        Body::GraphQl { query, variables } => {
            // Variables that are not valid JSON yet are still worth exporting as typed.
            let payload = Body::graphql_payload(query, variables).unwrap_or_else(|_| {
                let query = serde_json::Value::from(query.as_str());
                format!(r#"{{"query":{query},"variables":{}}}"#, variables.trim())
            });
            option("--data-raw", &payload);
        }
    }

    std::iter::once(first.join(" "))
        .chain(options)
        .collect::<Vec<_>>()
        .join(" \\\n  ")
}

fn full_url(request: &Request) -> String {
    let auth_query = request
        .auth
        .query()
        .filter(|(key, _)| !cornu_core::key_value::enabled(&request.query).any(|(k, _)| k == *key));
    let pairs: Vec<_> = cornu_core::key_value::enabled(&request.query)
        .chain(auth_query)
        .collect();
    if pairs.is_empty() {
        return request.url.clone();
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    let separator = if request.url.contains('?') { '&' } else { '?' };
    format!("{}{separator}{query}", request.url)
}

fn quote(text: &str) -> String {
    let safe = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=,%+".contains(c));
    if safe {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}
