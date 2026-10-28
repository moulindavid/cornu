pub mod curl;
pub mod discovery;
pub mod openapi;
pub mod postman;

use std::path::{Path, PathBuf};

use cornu_core::{Auth, KeyValue, Request};
use cornu_storage::{StorageError, Workspace};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Unsupported(String),
}

/// A collection read from another tool, not yet written to a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub root: ImportedFolder,
    /// Things that could not be imported faithfully, worth telling the user about.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedFolder {
    pub name: String,
    pub variables: Vec<KeyValue>,
    pub auth: Auth,
    pub items: Vec<ImportedItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedItem {
    Folder(ImportedFolder),
    Request(Request),
}

impl ImportedFolder {
    pub fn request_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| match item {
                ImportedItem::Folder(folder) => folder.request_count(),
                ImportedItem::Request(_) => 1,
            })
            .sum()
    }

    /// Writes this folder, and everything in it, as a new folder inside `parent`.
    pub fn write_into(
        &self,
        workspace: &mut Workspace,
        parent: &Path,
    ) -> Result<PathBuf, StorageError> {
        let path = workspace.create_folder(parent, &self.name)?;
        if let Some(folder) = workspace.folder(&path) {
            let mut settings = folder.settings.clone();
            settings.variables.clone_from(&self.variables);
            settings.auth = self.auth.clone();
            workspace.update_folder(&path, settings)?;
        }
        for item in &self.items {
            match item {
                ImportedItem::Folder(folder) => {
                    folder.write_into(workspace, &path)?;
                }
                ImportedItem::Request(request) => {
                    workspace.create_request(&path, request.clone())?;
                }
            }
        }
        Ok(path)
    }
}
