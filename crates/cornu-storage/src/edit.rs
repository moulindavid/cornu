use std::fs;
use std::path::{Path, PathBuf};

use cornu_core::Request;

use crate::secrets::Secrets;
use crate::{
    EnvironmentEntry, Folder, Item, RequestEntry, Settings, StorageError, Workspace, create_dir,
    entry_name, slugify,
};

impl Workspace {
    pub fn folder(&self, path: &Path) -> Option<&Folder> {
        fn find<'a>(folder: &'a Folder, path: &Path) -> Option<&'a Folder> {
            if folder.path == path {
                return Some(folder);
            }
            folder.items.iter().find_map(|item| match item {
                Item::Folder(child) if path.starts_with(&child.path) => find(child, path),
                _ => None,
            })
        }
        find(&self.tree, path)
    }

    pub fn folder_mut(&mut self, path: &Path) -> Option<&mut Folder> {
        fn find<'a>(folder: &'a mut Folder, path: &Path) -> Option<&'a mut Folder> {
            if folder.path == path {
                return Some(folder);
            }
            folder.items.iter_mut().find_map(|item| match item {
                Item::Folder(child) if path.starts_with(&child.path) => find(child, path),
                _ => None,
            })
        }
        find(&mut self.tree, path)
    }

    pub fn create_request(
        &mut self,
        parent: &Path,
        request: Request,
    ) -> Result<PathBuf, StorageError> {
        let entry = RequestEntry {
            path: self.new_request_path(parent, &request.name),
            request,
        };
        self.save_request(&entry)?;
        let path = entry.path.clone();
        self.insert(parent, None, Item::Request(entry))?;
        Ok(path)
    }

    pub fn create_folder(&mut self, parent: &Path, name: &str) -> Result<PathBuf, StorageError> {
        let folder = Folder {
            path: self.new_folder_path(parent, name),
            settings: Settings {
                name: name.to_owned(),
                ..Default::default()
            },
            items: Vec::new(),
        };
        self.save_folder(&folder)?;
        let path = folder.path.clone();
        self.insert(parent, None, Item::Folder(folder))?;
        Ok(path)
    }

    pub fn update_request(&mut self, path: &Path, request: Request) -> Result<(), StorageError> {
        let entry = RequestEntry {
            path: path.to_owned(),
            request,
        };
        self.save_request(&entry)?;
        if let Some(Item::Request(existing)) = self.item_mut(path) {
            *existing = entry;
        }
        Ok(())
    }

    pub fn update_folder(&mut self, path: &Path, settings: Settings) -> Result<(), StorageError> {
        let Some(folder) = self.folder_mut(path) else {
            return Ok(());
        };
        folder.settings = settings;
        let folder = folder.clone();
        self.save_folder(&folder)
    }

    /// Also moves the file or directory so its name keeps matching. Returns the new path.
    pub fn rename(&mut self, path: &Path, name: &str) -> Result<PathBuf, StorageError> {
        let Some(mut item) = self.item_mut(path).map(|item| item.clone()) else {
            return Ok(path.to_owned());
        };
        set_name(&mut item, name);
        let parent = parent_of(path);
        let new_path = if slugify(name) == entry_name(path) {
            path.to_owned()
        } else {
            match item {
                Item::Folder(_) => self.new_folder_path(&parent, name),
                Item::Request(_) => self.new_request_path(&parent, name),
            }
        };
        if new_path != path {
            rename_path(&self.root.join(path), &self.root.join(&new_path))?;
            rebase(&mut item, path, &new_path);
            if matches!(item, Item::Folder(_)) {
                self.move_folder_secrets(path, Some(&new_path))?;
            }
        }
        match &item {
            Item::Folder(folder) => self.save_folder(folder)?,
            Item::Request(entry) => self.save_request(entry)?,
        }
        if let Some(existing) = self.item_mut(path) {
            *existing = item;
        }
        if let Some(folder) = self.folder(&parent).cloned() {
            self.save_folder(&folder)?;
        }
        Ok(new_path)
    }

    /// The copy is placed right after the original. Returns its path.
    pub fn duplicate(&mut self, path: &Path) -> Result<PathBuf, StorageError> {
        let parent = parent_of(path);
        let Some(index) = self.position(path) else {
            return Ok(path.to_owned());
        };
        let Some(mut item) = self.item_mut(path).map(|item| item.clone()) else {
            return Ok(path.to_owned());
        };
        let name = format!("{} copy", item_name(&item));
        set_name(&mut item, &name);
        let new_path = match item {
            Item::Folder(_) => self.new_folder_path(&parent, &name),
            Item::Request(_) => self.new_request_path(&parent, &name),
        };
        rebase(&mut item, path, &new_path);
        self.save_item(&item)?;
        self.insert(&parent, Some(index + 1), item)?;
        Ok(new_path)
    }

    pub fn delete(&mut self, path: &Path) -> Result<(), StorageError> {
        let parent = parent_of(path);
        let (Some(index), Some(folder)) = (self.position(path), self.folder_mut(&parent)) else {
            return Ok(());
        };
        let item = folder.items.remove(index);
        let is_folder = matches!(item, Item::Folder(_));
        let full = self.root.join(path);
        let removed = match item {
            Item::Folder(_) => fs::remove_dir_all(&full),
            Item::Request(_) => fs::remove_file(&full),
        };
        removed.map_err(|source| StorageError::Io { path: full, source })?;
        if is_folder {
            self.move_folder_secrets(path, None)?;
        }
        match self.folder(&parent).cloned() {
            Some(folder) => self.save_folder(&folder),
            None => Ok(()),
        }
    }

    pub fn create_environment(&mut self, name: &str) -> Result<usize, StorageError> {
        let entry = EnvironmentEntry {
            path: self.new_environment_path(name),
            environment: cornu_core::Environment::new(name),
        };
        self.save_environment(&entry)?;
        self.environments.push(entry);
        Ok(self.environments.len() - 1)
    }

    pub fn delete_environment(&mut self, index: usize) -> Result<(), StorageError> {
        if index >= self.environments.len() {
            return Ok(());
        }
        let entry = self.environments.remove(index);
        let mut secrets = Secrets::load(&self.root)?;
        if secrets
            .environments
            .remove(&entry_name(&entry.path))
            .is_some()
        {
            secrets.save(&self.root)?;
        }
        let full = self.root.join(&entry.path);
        fs::remove_file(&full).map_err(|source| StorageError::Io { path: full, source })
    }

    fn move_folder_secrets(&self, from: &Path, to: Option<&Path>) -> Result<(), StorageError> {
        let mut secrets = Secrets::load(&self.root)?;
        if secrets.move_folders(from, to) {
            secrets.save(&self.root)?;
        }
        Ok(())
    }

    fn save_item(&self, item: &Item) -> Result<(), StorageError> {
        match item {
            Item::Request(entry) => self.save_request(entry),
            Item::Folder(folder) => {
                create_dir(&self.root.join(&folder.path))?;
                self.save_folder(folder)?;
                folder
                    .items
                    .iter()
                    .try_for_each(|child| self.save_item(child))
            }
        }
    }

    fn insert(
        &mut self,
        parent: &Path,
        index: Option<usize>,
        item: Item,
    ) -> Result<(), StorageError> {
        let Some(folder) = self.folder_mut(parent) else {
            return Ok(());
        };
        let index = index.unwrap_or(folder.items.len()).min(folder.items.len());
        folder.items.insert(index, item);
        let folder = folder.clone();
        self.save_folder(&folder)
    }

    fn position(&self, path: &Path) -> Option<usize> {
        self.folder(&parent_of(path))?
            .items
            .iter()
            .position(|item| item.path() == path)
    }

    fn item_mut(&mut self, path: &Path) -> Option<&mut Item> {
        self.folder_mut(&parent_of(path))?
            .items
            .iter_mut()
            .find(|item| item.path() == path)
    }
}

fn parent_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_owned).unwrap_or_default()
}

fn item_name(item: &Item) -> &str {
    match item {
        Item::Folder(folder) => &folder.settings.name,
        Item::Request(entry) => &entry.request.name,
    }
}

fn set_name(item: &mut Item, name: &str) {
    match item {
        Item::Folder(folder) => folder.settings.name = name.to_owned(),
        Item::Request(entry) => entry.request.name = name.to_owned(),
    }
}

fn rebase(item: &mut Item, from: &Path, to: &Path) {
    let moved = |path: &Path| moved_path(path, from, to).unwrap_or_else(|| path.to_owned());
    match item {
        Item::Request(entry) => entry.path = moved(&entry.path),
        Item::Folder(folder) => {
            folder.path = moved(&folder.path);
            for child in &mut folder.items {
                rebase(child, from, to);
            }
        }
    }
}

fn rename_path(from: &Path, to: &Path) -> Result<(), StorageError> {
    fs::rename(from, to).map_err(|source| StorageError::Io {
        path: from.to_owned(),
        source,
    })
}

/// Where `path` ends up once `from` has been moved to `to`, if it was inside `from`.
pub fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_owned()
    } else {
        to.join(rest)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_path_rewrites_prefixes_only() {
        let from = Path::new("users");
        let to = Path::new("people");
        assert_eq!(
            moved_path(Path::new("users/list.req.toml"), from, to),
            Some(PathBuf::from("people/list.req.toml"))
        );
        assert_eq!(moved_path(from, from, to), Some(to.to_owned()));
        assert_eq!(moved_path(Path::new("users-2/a.req.toml"), from, to), None);
    }
}
