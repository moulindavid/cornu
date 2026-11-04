use std::fs;
use std::path::{Path, PathBuf};

use cornu_core::CookieJar;

use crate::{LOCAL_DIR, StorageError, write_file};

pub const COOKIES_FILE: &str = "cookies.json";

pub fn cookies_path(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(COOKIES_FILE)
}

/// Replaces the jar's content with the workspace's cookies; no file means no cookies.
pub fn load_cookies(root: &Path, jar: &CookieJar) -> Result<(), StorageError> {
    let path = cookies_path(root);
    match fs::read_to_string(&path) {
        Ok(text) => jar.load_json(&text).map_err(|error| StorageError::Cookies {
            path,
            message: error.to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            jar.clear();
            Ok(())
        }
        Err(source) => Err(StorageError::Io { path, source }),
    }
}

pub fn save_cookies(root: &Path, jar: &CookieJar) -> Result<(), StorageError> {
    let path = cookies_path(root);
    let json = jar.to_json().map_err(|error| StorageError::Cookies {
        path: path.clone(),
        message: error.to_string(),
    })?;
    write_file(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let jar = CookieJar::default();
        load_cookies(dir.path(), &jar).unwrap();
        assert!(jar.is_empty());

        let url = url::Url::parse("https://api.example/").unwrap();
        jar.insert("session=abc", &url);
        save_cookies(dir.path(), &jar).unwrap();

        let restored = CookieJar::default();
        restored.insert("stale=1", &url);
        load_cookies(dir.path(), &restored).unwrap();
        assert_eq!(restored.header_for(&url).as_deref(), Some("session=abc"));

        fs::write(cookies_path(dir.path()), "garbage").unwrap();
        assert!(matches!(
            load_cookies(dir.path(), &restored),
            Err(StorageError::Cookies { .. })
        ));
    }
}
