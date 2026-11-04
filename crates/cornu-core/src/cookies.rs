use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use cookie_store::{CookieExpiration, CookieStore, RawCookie};
use reqwest::header::HeaderValue;
use url::Url;

/// Cookies set by responses and sent back on later requests, shared by every clone.
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    store: Arc<RwLock<CookieStore>>,
    /// Bumped on every change, so callers know when to persist.
    revision: Arc<AtomicU64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieInfo {
    pub domain: String,
    pub path: String,
    pub name: String,
    pub value: String,
    /// Unix time in seconds; `None` for a session cookie.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid cookie file: {0}")]
pub struct CookieFileError(String);

impl CookieJar {
    fn read(&self) -> RwLockReadGuard<'_, CookieStore> {
        self.store.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, CookieStore> {
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.store.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Unexpired cookies, sorted by domain, path and name.
    pub fn list(&self) -> Vec<CookieInfo> {
        let mut cookies: Vec<CookieInfo> = self
            .read()
            .iter_unexpired()
            .map(|cookie| CookieInfo {
                domain: cookie.domain.as_cow().unwrap_or_default().into_owned(),
                path: String::from(&cookie.path),
                name: cookie.name().to_owned(),
                value: cookie.value().to_owned(),
                expires: match cookie.expires {
                    CookieExpiration::AtUtc(at) => Some(at.unix_timestamp()),
                    CookieExpiration::SessionEnd => None,
                },
                secure: cookie.secure().unwrap_or(false),
                http_only: cookie.http_only().unwrap_or(false),
            })
            .collect();
        cookies.sort_by(|a, b| (&a.domain, &a.path, &a.name).cmp(&(&b.domain, &b.path, &b.name)));
        cookies
    }

    pub fn is_empty(&self) -> bool {
        self.read().iter_unexpired().next().is_none()
    }

    pub fn remove(&self, domain: &str, path: &str, name: &str) {
        self.write().remove(domain, path, name);
    }

    pub fn clear(&self) {
        self.write().clear();
    }

    /// `name=value; …` as it would be sent to `url`.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let store = self.read();
        let mut cookies = store.matches(url);
        // RFC 6265 5.4: longer paths first; then by name, as the store keeps no creation order.
        cookies.sort_by(|a, b| {
            let (a_path, b_path): (&str, &str) = (a.path.as_ref(), b.path.as_ref());
            b_path
                .len()
                .cmp(&a_path.len())
                .then_with(|| a.name().cmp(b.name()))
        });
        let pairs: Vec<String> = cookies
            .iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// Stores `set_cookie` as if a response from `url` had sent it.
    pub fn insert(&self, set_cookie: &str, url: &Url) -> bool {
        let Ok(cookie) = RawCookie::parse(set_cookie.to_owned()) else {
            return false;
        };
        self.write().insert_raw(&cookie, url).is_ok()
    }

    /// Session cookies are kept too: in an API client a "session" spans restarts.
    pub fn to_json(&self) -> Result<String, CookieFileError> {
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&self.read(), &mut bytes)
            .map_err(|error| CookieFileError(error.to_string()))?;
        String::from_utf8(bytes).map_err(|error| CookieFileError(error.to_string()))
    }

    /// Replaces the content of the jar; expired cookies are dropped.
    pub fn load_json(&self, text: &str) -> Result<(), CookieFileError> {
        let store = cookie_store::serde::json::load(text.as_bytes())
            .map_err(|error| CookieFileError(error.to_string()))?;
        *self.write() = store;
        Ok(())
    }
}

impl reqwest::cookie::CookieStore for CookieJar {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies: Vec<RawCookie<'static>> = headers
            .filter_map(|value| std::str::from_utf8(value.as_bytes()).ok())
            .filter_map(|text| RawCookie::parse(text.to_owned()).ok())
            .collect();
        if !cookies.is_empty() {
            self.write()
                .store_response_cookies(cookies.into_iter(), url);
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        HeaderValue::from_str(&self.header_for(url)?).ok()
    }
}
