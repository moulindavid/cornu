use std::fs;

use cornu_core::{Method, Request};
use cornu_storage::{HISTORY_LIMIT, History, HistoryEntry, Outcome};

fn entry(index: usize) -> HistoryEntry {
    HistoryEntry {
        timestamp: 1_700_000_000 + index as i64,
        request: Request::new(Method::Get, format!("{{{{base_url}}}}/items/{index}")),
        source: None,
        outcome: Outcome::Response {
            status: 200,
            elapsed_ms: 12,
            size: 34,
        },
    }
}

#[test]
fn entries_survive_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::load(dir.path()).unwrap();
    assert!(history.entries().is_empty());
    history.push(entry(0)).unwrap();
    let mut failed = entry(1);
    failed.source = Some("users/list.req.toml".into());
    failed.outcome = Outcome::Error {
        message: "connection refused".into(),
    };
    history.push(failed).unwrap();

    let reloaded = History::load(dir.path()).unwrap();
    assert_eq!(reloaded.entries(), history.entries());
    assert!(reloaded.entries()[0].request.url.contains("{{base_url}}"));
}

#[test]
fn broken_lines_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::load(dir.path()).unwrap();
    history.push(entry(0)).unwrap();
    let path = History::path(dir.path());
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"timestamp\": 12, \"requ");
    fs::write(&path, text).unwrap();

    assert_eq!(History::load(dir.path()).unwrap().entries().len(), 1);
}

#[test]
fn history_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::load(dir.path()).unwrap();
    for index in 0..HISTORY_LIMIT * 2 + 1 {
        history.push(entry(index)).unwrap();
    }
    assert_eq!(history.entries().len(), HISTORY_LIMIT);
    let reloaded = History::load(dir.path()).unwrap();
    assert_eq!(reloaded.entries().len(), HISTORY_LIMIT);
    assert_eq!(reloaded.entries().last(), Some(&entry(HISTORY_LIMIT * 2)));
}

#[test]
fn in_memory_history_is_capped_too() {
    let mut history = History::default();
    for index in 0..HISTORY_LIMIT + 3 {
        history.push(entry(index)).unwrap();
    }
    assert_eq!(history.entries().len(), HISTORY_LIMIT);
    assert_eq!(history.entries()[0], entry(3));
}

#[test]
fn clear_empties_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::load(dir.path()).unwrap();
    history.push(entry(0)).unwrap();
    history.clear().unwrap();
    assert!(History::load(dir.path()).unwrap().entries().is_empty());
}
