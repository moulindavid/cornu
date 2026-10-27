use std::path::{Path, PathBuf};

use cornu_core::{Auth, Environment, Variables};
use cornu_storage::{EnvironmentEntry, Folder, History, Settings, StorageError, Workspace};

/// The open workspace, or an in-memory scratch one when nothing is open.
pub struct Project {
    pub workspace: Workspace,
    pub history: History,
    on_disk: bool,
}

impl Project {
    pub fn scratch() -> Self {
        Self {
            workspace: Workspace {
                root: PathBuf::new(),
                tree: Folder {
                    path: PathBuf::new(),
                    settings: Settings {
                        name: "Scratch".into(),
                        ..Default::default()
                    },
                    items: Vec::new(),
                },
                environments: Vec::new(),
                problems: Vec::new(),
            },
            history: History::default(),
            on_disk: false,
        }
    }

    pub fn open(root: &Path) -> Result<Self, StorageError> {
        Ok(Self::on_disk(Workspace::load(root)?))
    }

    pub fn create(root: &Path) -> Result<Self, StorageError> {
        let name = root
            .file_name()
            .map_or("Workspace".into(), |name| name.to_string_lossy());
        Ok(Self::on_disk(Workspace::init(root, &name)?))
    }

    /// History is local convenience: failing to read it should not prevent opening.
    fn on_disk(mut workspace: Workspace) -> Self {
        let history = History::load(&workspace.root).unwrap_or_else(|error| {
            workspace.problems.push(error.to_string());
            History::default()
        });
        Self {
            workspace,
            history,
            on_disk: true,
        }
    }

    pub fn is_on_disk(&self) -> bool {
        self.on_disk
    }

    pub fn root(&self) -> Option<&Path> {
        self.on_disk.then_some(self.workspace.root.as_path())
    }

    pub fn name(&self) -> &str {
        &self.workspace.tree.settings.name
    }

    pub fn environment(&self, index: Option<usize>) -> Option<&Environment> {
        index
            .and_then(|index| self.workspace.environments.get(index))
            .map(|entry| &entry.environment)
    }

    /// Scratch requests only see the globals and the active environment.
    pub fn variables(&self, path: Option<&Path>, environment: Option<usize>) -> Variables {
        let path = path.unwrap_or(Path::new(""));
        self.workspace
            .variables(path, self.environment(environment))
    }

    pub fn effective_auth(&self, path: Option<&Path>, auth: &Auth) -> Auth {
        let path = path.unwrap_or(Path::new(""));
        self.workspace.effective_auth(path, auth)
    }

    pub fn save_folder(&mut self, path: &Path, settings: Settings) -> Result<(), StorageError> {
        if self.on_disk {
            self.workspace.update_folder(path, settings)
        } else {
            if let Some(folder) = self.workspace.folder_mut(path) {
                folder.settings = settings;
            }
            Ok(())
        }
    }

    pub fn save_environment(&self, index: usize) -> Result<(), StorageError> {
        match self.workspace.environments.get(index) {
            Some(entry) if self.on_disk => self.workspace.save_environment(entry),
            _ => Ok(()),
        }
    }

    pub fn add_environment(&mut self, environment: Environment) -> Result<usize, StorageError> {
        if self.on_disk {
            return self.workspace.create_environment(environment);
        }
        self.workspace.environments.push(EnvironmentEntry {
            path: PathBuf::new(),
            environment,
        });
        Ok(self.workspace.environments.len() - 1)
    }

    pub fn remove_environment(&mut self, index: usize) -> Result<(), StorageError> {
        if self.on_disk {
            self.workspace.delete_environment(index)
        } else {
            if index < self.workspace.environments.len() {
                self.workspace.environments.remove(index);
            }
            Ok(())
        }
    }
}
