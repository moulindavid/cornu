use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use reqwest::{multipart, redirect};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    Auth, Body, CookieJar, OAuth2, PartValue, Request, Response, Token, TokenStore, key_value,
};

/// Stored in the workspace's `cornu.toml`; relative paths are relative to the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientOptions {
    #[serde(with = "seconds", skip_serializing_if = "is_default_timeout")]
    pub timeout: Option<Duration>,
    #[serde(skip_serializing_if = "is_true")]
    pub follow_redirects: bool,
    #[serde(skip_serializing_if = "is_default_max_redirects")]
    pub max_redirects: usize,
    #[serde(skip_serializing_if = "ProxyMode::is_system")]
    pub proxy: ProxyMode,
    /// Accepts any certificate and host name: for self-signed development servers only.
    #[serde(skip_serializing_if = "is_false")]
    pub insecure: bool,
    /// PEM certificates trusted on top of the system ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_certificate: Option<PathBuf>,
    /// PEM file holding the client certificate chain and its private key, for mutual TLS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<PathBuf>,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_REDIRECTS: usize = 10;

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Some(DEFAULT_TIMEOUT),
            follow_redirects: true,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            proxy: ProxyMode::System,
            insecure: false,
            ca_certificate: None,
            client_certificate: None,
        }
    }
}

impl ClientOptions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// With certificate paths made absolute against `root`.
    pub fn relative_to(&self, root: &Path) -> Self {
        let mut options = self.clone();
        for path in [&mut options.ca_certificate, &mut options.client_certificate]
            .into_iter()
            .flatten()
        {
            *path = root.join(&*path);
        }
        options
    }
}

fn is_default_timeout(timeout: &Option<Duration>) -> bool {
    *timeout == Some(DEFAULT_TIMEOUT)
}

fn is_default_max_redirects(max: &usize) -> bool {
    *max == DEFAULT_MAX_REDIRECTS
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whole seconds, 0 meaning no timeout.
mod seconds {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        timeout: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(timeout.map_or(0, |timeout| timeout.as_secs()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let seconds = u64::deserialize(deserializer)?;
        Ok((seconds > 0).then(|| Duration::from_secs(seconds)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProxyMode {
    /// From the `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` environment variables.
    #[default]
    System,
    None,
    Custom {
        /// `http://`, `https://` or `socks5://`, with optional `user:password@`.
        url: String,
        /// Comma-separated hosts, domains or IP ranges reached directly.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        no_proxy: String,
    },
}

impl ProxyMode {
    pub fn is_system(&self) -> bool {
        matches!(self, ProxyMode::System)
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
    #[error("OAuth2: {0}")]
    OAuth(String),
    #[error("invalid proxy {url:?}: {message}")]
    InvalidProxy { url: String, message: String },
    #[error("invalid certificate {path}: {message}")]
    InvalidCertificate { path: PathBuf, message: String },
    #[error("invalid GraphQL variables: {0}")]
    InvalidGraphqlVariables(String),
    #[error("request timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
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
    /// Without the cookie jar, for requests that set their own `Cookie` header: reqwest would
    /// replace it with the jar's.
    plain: Option<reqwest::Client>,
    tokens: TokenStore,
}

fn builder(options: &ClientOptions) -> Result<reqwest::ClientBuilder, SendError> {
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
    builder = match &options.proxy {
        ProxyMode::System => builder,
        ProxyMode::None => builder.no_proxy(),
        ProxyMode::Custom { url, no_proxy } => {
            let invalid = |error: reqwest::Error| SendError::InvalidProxy {
                url: url.clone(),
                message: error_chain(&error),
            };
            let proxy = reqwest::Proxy::all(url.trim())
                .map_err(invalid)?
                .no_proxy(reqwest::NoProxy::from_string(no_proxy));
            builder.proxy(proxy)
        }
    };
    if options.insecure {
        builder = builder
            .tls_danger_accept_invalid_certs(true)
            .tls_danger_accept_invalid_hostnames(true);
    }
    if let Some(path) = &options.ca_certificate {
        let certificates = reqwest::Certificate::from_pem_bundle(&read_pem(path)?)
            .map_err(|error| invalid_certificate(path, &error))?;
        if certificates.is_empty() {
            return Err(SendError::InvalidCertificate {
                path: path.clone(),
                message: "no PEM certificate found".into(),
            });
        }
        builder = builder.tls_certs_merge(certificates);
    }
    if let Some(path) = &options.client_certificate {
        let identity = reqwest::Identity::from_pem(&read_pem(path)?)
            .map_err(|error| invalid_certificate(path, &error))?;
        builder = builder.identity(identity);
    }
    Ok(builder)
}

fn read_pem(path: &Path) -> Result<Vec<u8>, SendError> {
    std::fs::read(path).map_err(|source| SendError::ReadFile {
        path: path.to_owned(),
        source,
    })
}

fn invalid_certificate(path: &Path, error: &reqwest::Error) -> SendError {
    SendError::InvalidCertificate {
        path: path.to_owned(),
        message: error_chain(error),
    }
}

impl HttpClient {
    pub fn new(options: &ClientOptions) -> Result<Self, SendError> {
        Ok(Self {
            client: builder(options)?.build()?,
            plain: None,
            tokens: TokenStore::default(),
        })
    }

    /// Cookies set by responses, redirects included, are stored in `jar` and sent back.
    pub fn with_cookies(options: &ClientOptions, jar: CookieJar) -> Result<Self, SendError> {
        Ok(Self {
            client: builder(options)?
                .cookie_provider(std::sync::Arc::new(jar))
                .build()?,
            plain: Some(builder(options)?.build()?),
            tokens: TokenStore::default(),
        })
    }

    /// OAuth2 tokens are taken from and stored in `tokens`.
    pub fn with_tokens(mut self, tokens: TokenStore) -> Self {
        self.tokens = tokens;
        self
    }

    pub fn tokens(&self) -> &TokenStore {
        &self.tokens
    }

    /// Always asks the server, even when a valid token is stored. `open` gets the URL to show in
    /// a browser for the authorization code grant.
    pub async fn new_token(
        &self,
        config: &OAuth2,
        open: impl FnOnce(&Url),
    ) -> Result<Token, SendError> {
        crate::oauth2::new_token(&self.client, &self.tokens, config, open).await
    }

    pub async fn send(&self, request: &Request) -> Result<Response, SendError> {
        let authorized;
        let request = match &request.auth {
            Auth::OAuth2(config) => {
                let token = crate::oauth2::token(&self.client, &self.tokens, config).await?;
                authorized = request.clone().with_auth(Auth::Bearer {
                    token: token.access_token,
                });
                &authorized
            }
            _ => request,
        };
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

        let client = match &self.plain {
            Some(plain) if request.has_header("cookie") => plain,
            _ => &self.client,
        };
        let mut builder = client.request(method, url).headers(headers);
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
            Body::GraphQl { query, variables } => builder.body(
                Body::graphql_payload(query, variables)
                    .map_err(|error| SendError::InvalidGraphqlVariables(error.to_string()))?,
            ),
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
