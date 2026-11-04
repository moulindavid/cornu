use std::fs;
use std::path::{Path, PathBuf};

use cornu_core::TokenStore;

use crate::{LOCAL_DIR, StorageError, write_file};

pub const TOKENS_FILE: &str = "tokens.json";

pub fn tokens_path(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(TOKENS_FILE)
}

/// Replaces the store's content with the workspace's OAuth2 tokens; no file means no tokens.
pub fn load_tokens(root: &Path, tokens: &TokenStore) -> Result<(), StorageError> {
    let path = tokens_path(root);
    match fs::read_to_string(&path) {
        Ok(text) => tokens
            .load_json(&text)
            .map_err(|error| StorageError::Tokens {
                path,
                message: error.to_string(),
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokens.clear();
            Ok(())
        }
        Err(source) => Err(StorageError::Io { path, source }),
    }
}

pub fn save_tokens(root: &Path, tokens: &TokenStore) -> Result<(), StorageError> {
    let path = tokens_path(root);
    let json = tokens.to_json().map_err(|error| StorageError::Tokens {
        path: path.clone(),
        message: error.to_string(),
    })?;
    write_file(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use cornu_core::{OAuth2, Token};

    use super::*;

    #[test]
    fn tokens_survive_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let config = OAuth2 {
            token_url: "https://auth.example/token".into(),
            client_id: "app".into(),
            ..Default::default()
        };
        let token = Token {
            access_token: "abc".into(),
            refresh_token: None,
            expires_at: None,
        };
        let store = TokenStore::default();
        store.set(&config, token.clone());
        load_tokens(dir.path(), &store).unwrap();
        assert!(store.is_empty(), "no file clears the store");

        store.set(&config, token.clone());
        save_tokens(dir.path(), &store).unwrap();
        let restored = TokenStore::default();
        load_tokens(dir.path(), &restored).unwrap();
        assert_eq!(restored.get(&config), Some(token));

        fs::write(tokens_path(dir.path()), "garbage").unwrap();
        assert!(matches!(
            load_tokens(dir.path(), &restored),
            Err(StorageError::Tokens { .. })
        ));
    }
}
