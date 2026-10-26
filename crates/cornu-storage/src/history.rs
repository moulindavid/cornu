use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use cornu_core::Request;
use serde::{Deserialize, Serialize};

use crate::{LOCAL_DIR, StorageError, create_dir, write_file};

pub const HISTORY_FILE: &str = "history.jsonl";
pub const HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix time in seconds.
    pub timestamp: i64,
    /// As edited, before variable substitution, so it can be sent again.
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Response {
        status: u16,
        elapsed_ms: u64,
        size: u64,
    },
    Error {
        message: String,
    },
}

/// Oldest entry first. Kept in memory only when there is no workspace.
#[derive(Debug, Default)]
pub struct History {
    path: Option<PathBuf>,
    entries: Vec<HistoryEntry>,
}

impl History {
    pub fn path(root: &Path) -> PathBuf {
        root.join(LOCAL_DIR).join(HISTORY_FILE)
    }

    /// Unreadable lines (e.g. cut short by a crash) are skipped.
    pub fn load(root: &Path) -> Result<Self, StorageError> {
        let path = Self::path(root);
        let entries = match fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(StorageError::Io { path, source }),
        };
        let mut history = Self {
            path: Some(path),
            entries,
        };
        if history.entries.len() > HISTORY_LIMIT {
            history.compact()?;
        }
        Ok(history)
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn push(&mut self, entry: HistoryEntry) -> Result<(), StorageError> {
        let line = to_line(&entry);
        self.entries.push(entry);
        let Some(path) = &self.path else {
            self.entries = trimmed(std::mem::take(&mut self.entries));
            return Ok(());
        };
        // Appending keeps each send cheap; the file is rewritten only once it doubles the limit.
        if self.entries.len() > HISTORY_LIMIT * 2 {
            return self.compact();
        }
        let io_error = |source| StorageError::Io {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path.parent() {
            create_dir(parent)?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .map_err(io_error)
    }

    pub fn clear(&mut self) -> Result<(), StorageError> {
        self.entries.clear();
        match &self.path {
            Some(path) if path.exists() => write_file(path, b""),
            _ => Ok(()),
        }
    }

    fn compact(&mut self) -> Result<(), StorageError> {
        self.entries = trimmed(std::mem::take(&mut self.entries));
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text: String = self.entries.iter().map(to_line).collect();
        write_file(path, text.as_bytes())
    }
}

fn trimmed(mut entries: Vec<HistoryEntry>) -> Vec<HistoryEntry> {
    let excess = entries.len().saturating_sub(HISTORY_LIMIT);
    entries.drain(..excess);
    entries
}

fn to_line(entry: &HistoryEntry) -> String {
    let mut line = serde_json::to_string(entry).expect("history entries always serialize");
    line.push('\n');
    line
}
