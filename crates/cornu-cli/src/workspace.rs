use std::path::{Path, PathBuf};

use cornu_core::{Environment, KeyValue, Request};
use cornu_storage::{CONFIG_FILE, Folder, Item, REQUEST_SUFFIX, RequestEntry, Workspace};

/// The given folder, or the nearest one holding a `cornu.toml` from the current folder up.
pub fn find_root(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = explicit {
        return if root.join(CONFIG_FILE).is_file() {
            Ok(root.to_owned())
        } else {
            Err(format!(
                "{} is not a cornu workspace: it has no {CONFIG_FILE}",
                root.display()
            ))
        };
    }
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    current
        .ancestors()
        .find(|dir| dir.join(CONFIG_FILE).is_file())
        .map(Path::to_owned)
        .ok_or_else(|| {
            format!(
                "no cornu workspace in {} or its parents: run inside a folder with a {CONFIG_FILE}, or pass --workspace",
                current.display()
            )
        })
}

pub fn environment<'a>(
    workspace: &'a Workspace,
    name: Option<&str>,
) -> Result<Option<&'a Environment>, String> {
    let Some(name) = name else {
        return Ok(None);
    };
    let matches = |entry: &&cornu_storage::EnvironmentEntry| {
        entry.environment.name.eq_ignore_ascii_case(name)
            || entry
                .path
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(name))
    };
    match workspace.environments.iter().find(matches) {
        Some(entry) => Ok(Some(&entry.environment)),
        None => {
            let names: Vec<_> = workspace
                .environments
                .iter()
                .map(|entry| entry.environment.name.as_str())
                .collect();
            Err(if names.is_empty() {
                format!("no environment {name}: the workspace has none")
            } else {
                format!("no environment {name}, available: {}", names.join(", "))
            })
        }
    }
}

pub enum Target<'a> {
    Request(&'a RequestEntry),
    Folder(&'a Folder),
}

impl Target<'_> {
    pub fn requests(&self) -> Vec<&RequestEntry> {
        match self {
            Target::Request(entry) => vec![entry],
            Target::Folder(folder) => folder_requests(folder),
        }
    }
}

/// `path` is relative to the workspace, or to the current folder when it exists there; the
/// `.req.toml` suffix is optional.
pub fn target<'a>(workspace: &'a Workspace, path: Option<&str>) -> Result<Target<'a>, String> {
    let Some(path) = path else {
        return Ok(Target::Folder(&workspace.tree));
    };
    let relative = relative_to_workspace(&workspace.root, path);
    let with_suffix = PathBuf::from(format!("{}{REQUEST_SUFFIX}", relative.display()));
    if let Some(entry) = workspace
        .find_request(&relative)
        .or_else(|| workspace.find_request(&with_suffix))
    {
        return Ok(Target::Request(entry));
    }
    if relative.as_os_str().is_empty() {
        return Ok(Target::Folder(&workspace.tree));
    }
    workspace
        .folder(&relative)
        .map(Target::Folder)
        .ok_or_else(|| {
            format!("no request or folder {path} in the workspace: `cornu-cli list` shows them")
        })
}

fn relative_to_workspace(root: &Path, path: &str) -> PathBuf {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if let Ok(root) = root.canonicalize() {
        for local in [trimmed.to_owned(), format!("{trimmed}{REQUEST_SUFFIX}")] {
            if let Ok(full) = Path::new(&local).canonicalize()
                && let Ok(inside) = full.strip_prefix(&root)
            {
                return inside.to_owned();
            }
        }
    }
    trimmed
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

pub fn folder_requests(folder: &Folder) -> Vec<&RequestEntry> {
    let mut requests = Vec::new();
    for item in &folder.items {
        match item {
            Item::Request(entry) => requests.push(entry),
            Item::Folder(child) => requests.extend(folder_requests(child)),
        }
    }
    requests
}

/// The path shown and accepted on the command line: `users/get` for `users/get.req.toml`.
pub fn display_path(path: &Path) -> String {
    let text = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    text.strip_suffix(REQUEST_SUFFIX)
        .map(str::to_owned)
        .unwrap_or(text)
}

pub fn kind(request: &Request) -> &'static str {
    if cornu_core::is_websocket_url(&request.url) {
        "websocket"
    } else if cornu_core::is_grpc_url(&request.url) {
        "grpc"
    } else if matches!(request.body, cornu_core::Body::GraphQl { .. }) {
        "graphql"
    } else {
        "http"
    }
}

/// Hides the values of secret variables in what is printed about requests.
#[derive(Debug, Default)]
pub struct Masker {
    secrets: Vec<String>,
}

pub const HIDDEN: &str = "•••";

impl Masker {
    pub fn new(workspace: &Workspace, environment: Option<&Environment>) -> Self {
        fn collect<'a>(folder: &'a Folder, into: &mut Vec<&'a KeyValue>) {
            into.extend(&folder.settings.variables);
            for item in &folder.items {
                if let Item::Folder(child) = item {
                    collect(child, into);
                }
            }
        }
        let mut variables = Vec::new();
        collect(&workspace.tree, &mut variables);
        if let Some(environment) = environment {
            variables.extend(&environment.variables);
        }
        let mut secrets: Vec<String> = variables
            .into_iter()
            // Very short values would hide unrelated text.
            .filter(|variable| variable.secret && variable.value.chars().count() >= 4)
            .map(|variable| variable.value.clone())
            .collect();
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        Self { secrets }
    }

    pub fn mask(&self, text: &str) -> String {
        self.secrets
            .iter()
            .fold(text.to_owned(), |text, secret| text.replace(secret, HIDDEN))
    }
}
