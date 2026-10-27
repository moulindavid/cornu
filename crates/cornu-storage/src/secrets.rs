use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cornu_core::KeyValue;
use serde::{Deserialize, Serialize};

use crate::{LOCAL_DIR, StorageError, read_toml, write_toml};

type Values = BTreeMap<String, String>;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Secrets {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub globals: Values,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environments: BTreeMap<String, Values>,
    /// Keyed by the folder path, with `/` separators on every platform.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<String, Values>,
}

impl Secrets {
    fn path(root: &Path) -> PathBuf {
        root.join(LOCAL_DIR).join("secrets.toml")
    }

    pub fn load(root: &Path) -> Result<Self, StorageError> {
        let path = Self::path(root);
        if path.exists() {
            read_toml(&path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), StorageError> {
        write_toml(&Self::path(root), self)
    }

    /// Returns whether anything changed.
    pub fn set_folder(&mut self, path: &Path, values: Values) -> bool {
        let key = folder_key(path);
        if values.is_empty() {
            self.folders.remove(&key).is_some()
        } else {
            self.folders.insert(key, values.clone()) != Some(values)
        }
    }

    /// Moves the secrets of `from` and of every folder inside it to `to`.
    pub fn move_folders(&mut self, from: &Path, to: Option<&Path>) -> bool {
        let moved: Vec<_> = self
            .folders
            .keys()
            .filter(|key| Path::new(key).starts_with(from))
            .cloned()
            .collect();
        for key in &moved {
            let values = self.folders.remove(key).unwrap_or_default();
            if let Some(to) = to {
                let rest = Path::new(key).strip_prefix(from).unwrap_or(Path::new(""));
                self.folders.insert(folder_key(&to.join(rest)), values);
            }
        }
        !moved.is_empty()
    }
}

pub(crate) fn folder_key(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Returns the rows to write to the shared file (secret values blanked) and the secret values.
pub(crate) fn split(rows: &[KeyValue]) -> (Vec<KeyValue>, Values) {
    let mut values = Values::new();
    let rows = rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            if row.secret {
                values.insert(row.key.clone(), std::mem::take(&mut row.value));
            }
            row
        })
        .collect();
    (rows, values)
}

pub(crate) fn merge(rows: &mut [KeyValue], values: Option<&Values>) {
    for row in rows.iter_mut().filter(|row| row.secret) {
        row.value = values
            .and_then(|values| values.get(&row.key))
            .cloned()
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_merge_round_trip() {
        let rows = vec![
            KeyValue::new("host", "api.example"),
            KeyValue::new("token", "s3cret").secret(),
        ];
        let (shared, values) = split(&rows);
        assert_eq!(shared[0].value, "api.example");
        assert_eq!(shared[1].value, "");
        assert_eq!(values["token"], "s3cret");

        let mut restored = shared;
        merge(&mut restored, Some(&values));
        assert_eq!(restored, rows);
    }

    #[test]
    fn missing_secrets_become_empty() {
        let mut rows = vec![KeyValue::new("token", "stale").secret()];
        merge(&mut rows, None);
        assert_eq!(rows[0].value, "");
    }
}
