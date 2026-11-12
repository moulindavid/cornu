use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::{SendError, Variables};

pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:8910/callback";
/// A token this close to its expiry is renewed before use.
const EXPIRY_MARGIN: i64 = 30;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    #[default]
    ClientCredentials,
    /// Always with PKCE (RFC 7636).
    AuthorizationCode,
}

impl Grant {
    pub const ALL: [Grant; 2] = [Grant::ClientCredentials, Grant::AuthorizationCode];

    pub fn label(self) -> &'static str {
        match self {
            Grant::ClientCredentials => "Client credentials",
            Grant::AuthorizationCode => "Authorization code (PKCE)",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuth {
    /// HTTP Basic, `client_secret_basic`.
    #[default]
    Header,
    /// In the form body, `client_secret_post`.
    Body,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuth2 {
    #[serde(default)]
    pub grant: Grant,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_url: String,
    #[serde(default)]
    pub token_url: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    /// Empty means [`DEFAULT_REDIRECT_URI`]; port 0 picks a free port (RFC 8252 7.3).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub redirect_uri: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub client_auth: ClientAuth,
}

fn is_default(client_auth: &ClientAuth) -> bool {
    *client_auth == ClientAuth::default()
}

impl OAuth2 {
    pub(crate) fn fields_mut(&mut self) -> Vec<&mut String> {
        vec![
            &mut self.auth_url,
            &mut self.token_url,
            &mut self.client_id,
            &mut self.client_secret,
            &mut self.scope,
            &mut self.redirect_uri,
        ]
    }

    pub fn resolve(&self, variables: &Variables) -> OAuth2 {
        let mut resolved = self.clone();
        for field in resolved.fields_mut() {
            *field = variables.interpolate(field).text;
        }
        resolved
    }

    /// Identifies the tokens issued for this configuration.
    pub fn key(&self) -> String {
        let grant = match self.grant {
            Grant::ClientCredentials => "client_credentials",
            Grant::AuthorizationCode => "authorization_code",
        };
        [
            grant,
            self.token_url.trim(),
            self.client_id.trim(),
            self.scope.trim(),
        ]
        .join(" ")
    }

    fn redirect(&self) -> Result<Url, SendError> {
        let raw = match self.redirect_uri.trim() {
            "" => DEFAULT_REDIRECT_URI,
            raw => raw,
        };
        let url =
            Url::parse(raw).map_err(|error| oauth(format!("invalid redirect URI: {error}")))?;
        match url.host_str() {
            Some("127.0.0.1" | "localhost" | "[::1]") if url.scheme() == "http" => Ok(url),
            _ => Err(oauth(
                "the redirect URI must be http://127.0.0.1, localhost or [::1]: cornu listens there",
            )),
        }
    }

    fn authorization_url(
        &self,
        redirect: &Url,
        challenge: &str,
        state: &str,
    ) -> Result<Url, SendError> {
        let mut url = Url::parse(self.auth_url.trim())
            .map_err(|error| oauth(format!("invalid authorization URL: {error}")))?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", self.client_id.trim())
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        if !self.scope.trim().is_empty() {
            query.append_pair("scope", self.scope.trim());
        }
        drop(query);
        Ok(url)
    }
}

fn oauth(message: impl Into<String>) -> SendError {
    SendError::OAuth(message.into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix time in seconds; `None` when the server gave no lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl Token {
    pub fn is_fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| unix_now() + EXPIRY_MARGIN < expires_at)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Tokens by [`OAuth2::key`], shared by every clone.
#[derive(Debug, Clone, Default)]
pub struct TokenStore {
    tokens: Arc<RwLock<BTreeMap<String, Token>>>,
    revision: Arc<AtomicU64>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid token file: {0}")]
pub struct TokenFileError(String);

impl TokenStore {
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn get(&self, config: &OAuth2) -> Option<Token> {
        let tokens = self.tokens.read().unwrap_or_else(PoisonError::into_inner);
        tokens.get(&config.key()).cloned()
    }

    pub fn len(&self) -> usize {
        self.tokens
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn update(&self, change: impl FnOnce(&mut BTreeMap<String, Token>)) {
        change(&mut self.tokens.write().unwrap_or_else(PoisonError::into_inner));
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set(&self, config: &OAuth2, token: Token) {
        self.update(|tokens| {
            tokens.insert(config.key(), token);
        });
    }

    pub fn remove(&self, config: &OAuth2) {
        self.update(|tokens| {
            tokens.remove(&config.key());
        });
    }

    pub fn clear(&self) {
        self.update(BTreeMap::clear);
    }

    pub fn to_json(&self) -> Result<String, TokenFileError> {
        let tokens = self.tokens.read().unwrap_or_else(PoisonError::into_inner);
        serde_json::to_string_pretty(&*tokens).map_err(|error| TokenFileError(error.to_string()))
    }

    /// Replaces the content of the store.
    pub fn load_json(&self, text: &str) -> Result<(), TokenFileError> {
        let loaded: BTreeMap<String, Token> =
            serde_json::from_str(text).map_err(|error| TokenFileError(error.to_string()))?;
        self.update(|tokens| *tokens = loaded);
        Ok(())
    }
}

/// A fresh stored token, else a refreshed one, else a new one for client credentials.
pub(crate) async fn token(
    client: &reqwest::Client,
    store: &TokenStore,
    config: &OAuth2,
) -> Result<Token, SendError> {
    let stored = store.get(config);
    if let Some(token) = &stored
        && token.is_fresh()
    {
        return Ok(token.clone());
    }
    if let Some(refresh_token) = stored.and_then(|token| token.refresh_token) {
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ];
        if let Ok(mut token) = request_token(client, config, &params).await {
            if token.refresh_token.is_none() {
                token.refresh_token = Some(refresh_token);
            }
            store.set(config, token.clone());
            return Ok(token);
        }
        store.remove(config);
    }
    match config.grant {
        Grant::ClientCredentials => new_token(client, store, config, |_| {}).await,
        Grant::AuthorizationCode => Err(oauth(
            "no valid token: get one from the Auth tab of the request in cornu first",
        )),
    }
}

/// Asks the server for a new token and stores it. For the authorization code grant, `open` gets
/// the URL to show in a browser, once cornu listens for the redirect.
pub(crate) async fn new_token(
    client: &reqwest::Client,
    store: &TokenStore,
    config: &OAuth2,
    open: impl FnOnce(&Url),
) -> Result<Token, SendError> {
    let token = match config.grant {
        Grant::ClientCredentials => {
            let mut params = vec![("grant_type", "client_credentials")];
            if !config.scope.trim().is_empty() {
                params.push(("scope", config.scope.trim()));
            }
            request_token(client, config, &params).await?
        }
        Grant::AuthorizationCode => authorization_code(client, config, open).await?,
    };
    store.set(config, token.clone());
    Ok(token)
}

async fn authorization_code(
    client: &reqwest::Client,
    config: &OAuth2,
    open: impl FnOnce(&Url),
) -> Result<Token, SendError> {
    let mut redirect = config.redirect()?;
    let host = redirect
        .host_str()
        .unwrap_or("127.0.0.1")
        .trim_matches(['[', ']']);
    let port = redirect.port_or_known_default().unwrap_or(80);
    let listener = TcpListener::bind((host, port))
        .await
        .map_err(|error| oauth(format!("cannot listen on {host}:{port}: {error}")))?;
    if port == 0 {
        let port = listener
            .local_addr()
            .map_err(|error| oauth(error.to_string()))?
            .port();
        let _ = redirect.set_port(Some(port));
    }

    let verifier = random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random();
    open(&config.authorization_url(&redirect, &challenge, &state)?);

    let code = tokio::time::timeout(CALLBACK_TIMEOUT, callback(&listener, &redirect, &state))
        .await
        .map_err(|_| oauth("no answer from the browser within 5 minutes"))??;
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    // A public client has no secret to authenticate with, so it names itself in the body.
    if config.client_secret.is_empty() {
        params.push(("client_id", config.client_id.trim()));
    }
    request_token(client, config, &params).await
}

/// 32 random bytes from the OS, base64url-encoded into the 43 characters RFC 7636 asks for.
fn random() -> String {
    let bytes: Vec<u8> = (0..2)
        .flat_map(|_| uuid::Uuid::new_v4().into_bytes())
        .collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Waits for the browser to come back to `redirect`, then returns the authorization code.
async fn callback(
    listener: &TcpListener,
    redirect: &Url,
    state: &str,
) -> Result<String, SendError> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|error| oauth(error.to_string()))?;
        let mut buffer = vec![0; 8192];
        let mut read = 0;
        while read < buffer.len() {
            match stream.read(&mut buffer[read..]).await {
                Ok(0) | Err(_) => break,
                Ok(count) => read += count,
            }
            if buffer[..read]
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buffer[..read]);
        let target = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default();
        let Ok(url) = redirect.join(target) else {
            continue;
        };
        if url.path() != redirect.path() {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let params: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
        let result = match (params.get("code"), params.get("error")) {
            _ if params.get("state").map(String::as_str) != Some(state) => Err(oauth(
                "the redirect has the wrong state, the flow was not started here",
            )),
            (_, Some(error)) => Err(oauth(match params.get("error_description") {
                Some(description) => format!("{error}: {description}"),
                None => error.clone(),
            })),
            (Some(code), None) => Ok(code.clone()),
            (None, None) => Err(oauth("the redirect has no code")),
        };
        let message = match &result {
            Ok(_) => "Authorized. You can close this tab and go back to cornu.".to_owned(),
            Err(error) => format!("Authorization failed: {error}"),
        };
        let page = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>cornu</title><p>{}</p>",
            message.replace('&', "&amp;").replace('<', "&lt;")
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        return result;
    }
}

async fn request_token(
    client: &reqwest::Client,
    config: &OAuth2,
    params: &[(&str, &str)],
) -> Result<Token, SendError> {
    let url = config.token_url.trim();
    if url.is_empty() {
        return Err(oauth("no token URL"));
    }
    let mut form: Vec<(&str, &str)> = params.to_vec();
    let mut builder = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded");
    let (id, secret) = (config.client_id.trim(), config.client_secret.as_str());
    match config.client_auth {
        _ if secret.is_empty() => {}
        ClientAuth::Header => {
            // RFC 6749 2.3.1: both parts are form-encoded before going into Basic.
            let encode = |text: &str| {
                url::form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>()
            };
            let credentials = STANDARD.encode(format!("{}:{}", encode(id), encode(secret)));
            builder = builder.header("Authorization", format!("Basic {credentials}"));
        }
        ClientAuth::Body => {
            form.extend([("client_id", id), ("client_secret", secret)]);
        }
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    let response = builder.body(body).send().await?;
    let status = response.status();
    let body = response.bytes().await?;
    let fields = parse_fields(&body);
    let field = |name: &str| fields.get(name).filter(|value| !value.is_empty()).cloned();
    if !status.is_success() {
        return Err(oauth(match (field("error"), field("error_description")) {
            (Some(error), Some(description)) => format!("{error}: {description}"),
            (Some(error), None) => error,
            _ => format!("the token endpoint answered {status}"),
        }));
    }
    let access_token = field("access_token")
        .ok_or_else(|| oauth("the token endpoint answered without an access_token"))?;
    Ok(Token {
        access_token,
        refresh_token: field("refresh_token"),
        expires_at: field("expires_in")
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .map(|seconds| unix_now() + seconds),
    })
}

/// JSON, or form-encoded as some servers still answer.
fn parse_fields(body: &[u8]) -> BTreeMap<String, String> {
    match serde_json::from_slice::<serde_json::Map<String, Value>>(body) {
        Ok(object) => object
            .into_iter()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                (key, text)
            })
            .collect(),
        Err(_) => url::form_urlencoded::parse(body).into_owned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_and_state_are_random_and_rfc_7636_sized() {
        let (a, b) = (random(), random());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }

    #[test]
    fn challenge_matches_the_rfc_7636_example() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn redirect_must_stay_on_the_loopback() {
        let mut config = OAuth2::default();
        assert_eq!(config.redirect().unwrap().as_str(), DEFAULT_REDIRECT_URI);
        config.redirect_uri = "https://example.com/callback".into();
        assert!(config.redirect().is_err());
    }

    #[test]
    fn freshness_keeps_a_margin() {
        let token = |expires_at| Token {
            access_token: "a".into(),
            refresh_token: None,
            expires_at,
        };
        assert!(token(None).is_fresh());
        assert!(token(Some(unix_now() + 3600)).is_fresh());
        assert!(!token(Some(unix_now() + 10)).is_fresh());
    }

    #[test]
    fn store_round_trips_through_json() {
        let config = OAuth2 {
            token_url: "https://auth.example/token".into(),
            client_id: "app".into(),
            ..Default::default()
        };
        let store = TokenStore::default();
        let token = Token {
            access_token: "abc".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(4_120_936_717),
        };
        store.set(&config, token.clone());
        let restored = TokenStore::default();
        restored.load_json(&store.to_json().unwrap()).unwrap();
        assert_eq!(restored.get(&config), Some(token));
        assert!(restored.load_json("[]").is_err());
    }
}
