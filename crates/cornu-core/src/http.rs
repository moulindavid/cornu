use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{multipart, redirect};
use url::Url;

use crate::{Body, PartValue, Request, Response, key_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOptions {
    pub timeout: Option<Duration>,
    pub follow_redirects: bool,
    pub max_redirects: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(30)),
            follow_redirects: true,
            max_redirects: 10,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("invalid URL {url:?}: {source}")]
    InvalidUrl {
        url: String,
        source: url::ParseError,
    },
    #[error("invalid header name {0:?}")]
    InvalidHeaderName(String),
    #[error("invalid value for header {0:?}")]
    InvalidHeaderValue(String),
    #[error("invalid content type {0:?}")]
    InvalidContentType(String),
    #[error("cannot read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("request timed out")]
    Timeout,
    #[error("connection failed: {0}")]
    Connect(String),
    #[error("{0}")]
    Http(String),
}

impl From<reqwest::Error> for SendError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            SendError::Timeout
        } else if error.is_connect() {
            SendError::Connect(error_chain(&error))
        } else {
            SendError::Http(error_chain(&error))
        }
    }
}

fn error_chain(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[derive(Debug, Clone)]
pub struct HttpClient {
    client: reqwest::Client,
}

impl HttpClient {
    pub fn new(options: &ClientOptions) -> Result<Self, SendError> {
        let redirect = if options.follow_redirects {
            redirect::Policy::limited(options.max_redirects)
        } else {
            redirect::Policy::none()
        };
        let mut builder = reqwest::Client::builder()
            .redirect(redirect)
            .user_agent(concat!("cornu/", env!("CARGO_PKG_VERSION")));
        if let Some(timeout) = options.timeout {
            builder = builder.timeout(timeout);
        }
        Ok(Self {
            client: builder.build()?,
        })
    }

    pub async fn send(&self, request: &Request) -> Result<Response, SendError> {
        let url = build_url(request)?;
        let mut headers = build_headers(request)?;
        let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
            .map_err(|_| SendError::Http(format!("invalid method {}", request.method)))?;

        if !request.has_header("content-type")
            && !matches!(request.body, Body::Multipart { .. })
            && let Some(content_type) = request.body.content_type()
        {
            let value = HeaderValue::from_str(content_type)
                .map_err(|_| SendError::InvalidContentType(content_type.to_owned()))?;
            headers.insert(CONTENT_TYPE, value);
        }

        let mut builder = self.client.request(method, url).headers(headers);
        builder = match &request.body {
            Body::None => builder,
            Body::Raw { text, .. } | Body::Json { text } => builder.body(text.clone()),
            Body::FormUrlEncoded { fields } => {
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(key_value::enabled(fields))
                    .finish();
                builder.body(encoded)
            }
            Body::Multipart { parts } => builder.multipart(build_multipart(parts).await?),
            Body::Binary { path, .. } => builder.body(read_file(path).await?),
        };

        let started = Instant::now();
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        let body = response.bytes().await?.to_vec();

        Ok(Response {
            status,
            headers,
            body,
            elapsed: started.elapsed(),
        })
    }
}

fn build_url(request: &Request) -> Result<Url, SendError> {
    let raw = request.url.trim();
    let with_scheme = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|source| SendError::InvalidUrl {
        url: raw.to_owned(),
        source,
    })?;
    let auth_query = request
        .auth
        .query()
        .filter(|(key, _)| !key_value::enabled(&request.query).any(|(k, _)| k == *key));
    let mut query = key_value::enabled(&request.query)
        .chain(auth_query)
        .peekable();
    if query.peek().is_some() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}

fn build_headers(request: &Request) -> Result<HeaderMap, SendError> {
    let mut headers = HeaderMap::new();
    for (key, value) in key_value::enabled(&request.headers) {
        let (name, value) = parse_header(key, value)?;
        headers.append(name, value);
    }
    if let Some((key, value)) = request.auth.header()
        && !request.has_header(&key)
    {
        let (name, value) = parse_header(&key, &value)?;
        headers.append(name, value);
    }
    Ok(headers)
}

fn parse_header(key: &str, value: &str) -> Result<(HeaderName, HeaderValue), SendError> {
    let name = HeaderName::from_bytes(key.trim().as_bytes())
        .map_err(|_| SendError::InvalidHeaderName(key.to_owned()))?;
    let value =
        HeaderValue::from_str(value).map_err(|_| SendError::InvalidHeaderValue(key.to_owned()))?;
    Ok((name, value))
}

async fn build_multipart(parts: &[crate::MultipartPart]) -> Result<multipart::Form, SendError> {
    let mut form = multipart::Form::new();
    for part in parts.iter().filter(|part| part.enabled) {
        let mut built = match &part.value {
            PartValue::Text(text) => multipart::Part::text(text.clone()),
            PartValue::File(path) => {
                let mut file = multipart::Part::bytes(read_file(path).await?);
                if let Some(name) = path.file_name() {
                    file = file.file_name(name.to_string_lossy().into_owned());
                }
                file
            }
        };
        if let Some(content_type) = &part.content_type {
            built = built
                .mime_str(content_type)
                .map_err(|_| SendError::InvalidContentType(content_type.clone()))?;
        }
        form = form.part(part.name.clone(), built);
    }
    Ok(form)
}

async fn read_file(path: &Path) -> Result<Vec<u8>, SendError> {
    tokio::fs::read(path)
        .await
        .map_err(|source| SendError::ReadFile {
            path: path.to_owned(),
            source,
        })
}
