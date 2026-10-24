mod secrets;
mod slug;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cornu_core::{Auth, Environment, KeyValue, Request, Variables};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use secrets::Secrets;
pub use slug::slugify;

pub const CONFIG_FILE: &str = "cornu.toml";
pub const FOLDER_FILE: &str = "_folder.toml";
pub const REQUEST_SUFFIX: &str = ".req.toml";
pub const ENVIRONMENTS_DIR: &str = "environments";
pub const LOCAL_DIR: &str = ".cornu";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{}: {source}", path.display())]
    Serialize {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("{} is not a cornu workspace (no {CONFIG_FILE})", .0.display())]
    NotAWorkspace(PathBuf),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Auth::is_inherit")]
    pub auth: Auth,
    /// Only used on disk: once loaded, the position of `Folder::items` is the order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Relative to the workspace root; empty for the root itself.
    pub path: PathBuf,
    pub settings: Settings,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Folder(Folder),
    Request(RequestEntry),
}

impl Item {
    pub fn path(&self) -> &Path {
        match self {
            Item::Folder(folder) => &folder.path,
            Item::Request(entry) => &entry.path,
        }
    }

    pub fn entry_name(&self) -> String {
        entry_name(self.path())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEntry {
    pub path: PathBuf,
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentEntry {
    pub path: PathBuf,
    pub environment: Environment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub root: PathBuf,
    pub tree: Folder,
    pub environments: Vec<EnvironmentEntry>,
    /// Files that could not be read; the rest of the workspace still loads.
    pub problems: Vec<String>,
}

impl Workspace {
    pub fn init(root: impl Into<PathBuf>, name: &str) -> Result<Self, StorageError> {
        let root = root.into();
        let config = root.join(CONFIG_FILE);
        if !config.exists() {
            let settings = Settings {
                name: name.to_owned(),
                ..Default::default()
            };
            write_toml(&config, &settings)?;
        }
        create_dir(&root.join(ENVIRONMENTS_DIR))?;
        ensure_gitignore(&root)?;
        Self::load(root)
    }

    pub fn load(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        let config = root.join(CONFIG_FILE);
        if !config.exists() {
            return Err(StorageError::NotAWorkspace(root));
        }
        let secrets = Secrets::load(&root)?;
        let mut settings: Settings = read_toml(&config)?;
        secrets::merge(&mut settings.variables, Some(&secrets.globals));

        let mut problems = Vec::new();
        let items = load_items(&root, Path::new(""), &settings.order, &mut problems)?;
        settings.order.clear();
        let environments = load_environments(&root, &secrets, &mut problems);
        Ok(Self {
            root,
            tree: Folder {
                path: PathBuf::new(),
                settings,
                items,
            },
            environments,
            problems,
        })
    }

    pub fn save_request(&self, entry: &RequestEntry) -> Result<(), StorageError> {
        write_toml(&self.root.join(&entry.path), &entry.request)
    }

    /// Also records the current item order of the folder.
    pub fn save_folder(&self, folder: &Folder) -> Result<(), StorageError> {
        let mut settings = folder.settings.clone();
        settings.order = folder.items.iter().map(Item::entry_name).collect();
        if folder.path.as_os_str().is_empty() {
            let (variables, values) = secrets::split(&settings.variables);
            settings.variables = variables;
            let mut secrets = Secrets::load(&self.root)?;
            secrets.globals = values;
            secrets.save(&self.root)?;
            write_toml(&self.root.join(CONFIG_FILE), &settings)
        } else {
            write_toml(&self.root.join(&folder.path).join(FOLDER_FILE), &settings)
        }
    }

    pub fn save_environment(&self, entry: &EnvironmentEntry) -> Result<(), StorageError> {
        let mut environment = entry.environment.clone();
        let (variables, values) = secrets::split(&environment.variables);
        environment.variables = variables;
        let mut secrets = Secrets::load(&self.root)?;
        let key = entry_name(&entry.path);
        if values.is_empty() {
            secrets.environments.remove(&key);
        } else {
            secrets.environments.insert(key, values);
        }
        secrets.save(&self.root)?;
        write_toml(&self.root.join(&entry.path), &environment)
    }

    pub fn new_request_path(&self, parent: &Path, name: &str) -> PathBuf {
        self.unique_path(parent, name, REQUEST_SUFFIX)
    }

    pub fn new_folder_path(&self, parent: &Path, name: &str) -> PathBuf {
        self.unique_path(parent, name, "")
    }

    pub fn new_environment_path(&self, name: &str) -> PathBuf {
        self.unique_path(Path::new(ENVIRONMENTS_DIR), name, ".toml")
    }

    fn unique_path(&self, parent: &Path, name: &str, suffix: &str) -> PathBuf {
        let slug = slugify(name);
        let taken = |candidate: &str| {
            let dir = self.root.join(parent);
            dir.join(candidate).exists()
                || dir.join(format!("{candidate}{REQUEST_SUFFIX}")).exists()
                || dir.join(format!("{candidate}.toml")).exists()
        };
        let mut candidate = slug.clone();
        let mut counter = 2;
        while taken(&candidate) {
            candidate = format!("{slug}-{counter}");
            counter += 1;
        }
        parent.join(format!("{candidate}{suffix}"))
    }

    pub fn find_request(&self, path: &Path) -> Option<&RequestEntry> {
        fn find<'a>(folder: &'a Folder, path: &Path) -> Option<&'a RequestEntry> {
            folder.items.iter().find_map(|item| match item {
                Item::Request(entry) if entry.path == path => Some(entry),
                Item::Folder(child) if path.starts_with(&child.path) => find(child, path),
                _ => None,
            })
        }
        find(&self.tree, path)
    }

    /// From the root down to the folder that directly contains `path`.
    pub fn folder_chain(&self, path: &Path) -> Vec<&Folder> {
        let mut chain = vec![&self.tree];
        let mut current = &self.tree;
        while let Some(child) = current.items.iter().find_map(|item| match item {
            Item::Folder(child) if path.starts_with(&child.path) && path != child.path => {
                Some(child)
            }
            _ => None,
        }) {
            chain.push(child);
            current = child;
        }
        chain
    }

    pub fn effective_auth(&self, entry: &RequestEntry) -> Auth {
        let chain = self.folder_chain(&entry.path);
        Auth::effective(
            std::iter::once(&entry.request.auth)
                .chain(chain.iter().rev().map(|folder| &folder.settings.auth)),
        )
    }

    /// Same precedence as Postman: root < folders (outer to inner) < environment.
    pub fn variables(&self, path: &Path, environment: Option<&Environment>) -> Variables {
        let mut variables = Variables::new();
        for folder in self.folder_chain(path) {
            variables.push_layer(cornu_core::key_value::enabled(&folder.settings.variables));
        }
        if let Some(environment) = environment {
            variables.push_layer(environment.enabled());
        }
        variables
    }
}

fn load_items(
    root: &Path,
    dir: &Path,
    order: &[String],
    problems: &mut Vec<String>,
) -> Result<Vec<Item>, StorageError> {
    let full = root.join(dir);
    let entries = fs::read_dir(&full).map_err(|source| StorageError::Io {
        path: full.clone(),
        source,
    })?;
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let path = dir.join(&file_name);
        if file_name.starts_with('.')
            || (dir.as_os_str().is_empty() && file_name == ENVIRONMENTS_DIR)
        {
            continue;
        }
        if entry.path().is_dir() {
            match load_folder(root, &path, problems) {
                Ok(folder) => items.push(Item::Folder(folder)),
                Err(error) => problems.push(error.to_string()),
            }
        } else if file_name.ends_with(REQUEST_SUFFIX) {
            match read_toml::<Request>(&entry.path()) {
                Ok(request) => items.push(Item::Request(RequestEntry { path, request })),
                Err(error) => problems.push(error.to_string()),
            }
        }
    }
    items.sort_by_cached_key(|item| {
        let name = item.entry_name();
        let position = order.iter().position(|o| *o == name).unwrap_or(usize::MAX);
        (position, name)
    });
    Ok(items)
}

fn load_folder(
    root: &Path,
    dir: &Path,
    problems: &mut Vec<String>,
) -> Result<Folder, StorageError> {
    let settings_path = root.join(dir).join(FOLDER_FILE);
    let mut settings = if settings_path.exists() {
        read_toml::<Settings>(&settings_path).unwrap_or_else(|error| {
            problems.push(error.to_string());
            Settings::default()
        })
    } else {
        Settings::default()
    };
    if settings.name.is_empty() {
        settings.name = entry_name(dir);
    }
    let items = load_items(root, dir, &settings.order, problems)?;
    settings.order.clear();
    Ok(Folder {
        path: dir.to_owned(),
        settings,
        items,
    })
}

fn load_environments(
    root: &Path,
    secrets: &Secrets,
    problems: &mut Vec<String>,
) -> Vec<EnvironmentEntry> {
    let Ok(entries) = fs::read_dir(root.join(ENVIRONMENTS_DIR)) else {
        return Vec::new();
    };
    let mut environments: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".toml"))
        .filter_map(|entry| {
            let path = Path::new(ENVIRONMENTS_DIR).join(entry.file_name());
            match read_toml::<Environment>(&entry.path()) {
                Ok(mut environment) => {
                    let values = secrets.environments.get(&entry_name(&path));
                    secrets::merge(&mut environment.variables, values);
                    Some(EnvironmentEntry { path, environment })
                }
                Err(error) => {
                    problems.push(error.to_string());
                    None
                }
            }
        })
        .collect();
    environments.sort_by(|a, b| a.environment.name.cmp(&b.environment.name));
    environments
}

fn entry_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(REQUEST_SUFFIX)
        .or_else(|| name.strip_suffix(".toml"))
        .unwrap_or(&name)
        .to_owned()
}

fn ensure_gitignore(root: &Path) -> Result<(), StorageError> {
    let path = root.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let ignored = existing
        .lines()
        .any(|line| matches!(line.trim(), ".cornu" | ".cornu/" | "/.cornu" | "/.cornu/"));
    if ignored {
        return Ok(());
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("# cornu local state: history and secrets\n.cornu/\n");
    write_file(&path, content.as_bytes())
}

pub(crate) fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T, StorageError> {
    let text = fs::read_to_string(path).map_err(|source| StorageError::Io {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| StorageError::Parse {
        path: path.to_owned(),
        source,
    })
}

pub(crate) fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let text = toml::to_string_pretty(value).map_err(|source| StorageError::Serialize {
        path: path.to_owned(),
        source,
    })?;
    write_file(path, text.as_bytes())
}

/// Writes through a temporary file so a crash never leaves a half-written file behind.
fn write_file(path: &Path, content: &[u8]) -> Result<(), StorageError> {
    let io_error = |source| StorageError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(io_error)?;
    fs::rename(&tmp, path).map_err(io_error)
}

fn create_dir(path: &Path) -> Result<(), StorageError> {
    fs::create_dir_all(path).map_err(|source| StorageError::Io {
        path: path.to_owned(),
        source,
    })
}
