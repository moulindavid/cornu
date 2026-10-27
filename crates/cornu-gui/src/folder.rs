use std::path::Path;

use eframe::egui;

use crate::editor::auth_editor;
use crate::project::Project;
use crate::widgets::{count_label, variable_table};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FolderTab {
    #[default]
    Auth,
    Variables,
}

/// Edits are saved as they happen; returns an error message if saving failed.
pub fn folder_view(
    ui: &mut egui::Ui,
    project: &mut Project,
    path: &Path,
    tab: &mut FolderTab,
) -> Option<String> {
    let Some(folder) = project.workspace.folder(path) else {
        ui.weak("This folder no longer exists.");
        return None;
    };
    let is_root = path.as_os_str().is_empty();
    let mut settings = folder.settings.clone();

    ui.heading(if is_root {
        format!("{} settings", settings.name)
    } else {
        format!("📁 {}", settings.name)
    });
    ui.weak(if is_root {
        "Auth and variables here apply to every request of the workspace."
    } else {
        "Auth and variables here apply to every request in this folder."
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.selectable_value(tab, FolderTab::Auth, "Auth");
        let variables = count_label("Variables", &settings.variables);
        ui.selectable_value(tab, FolderTab::Variables, variables);
    });
    ui.separator();

    let id = egui::Id::unique(("folder", path));
    egui::ScrollArea::vertical()
        .id_salt(id)
        .show(ui, |ui| match tab {
            FolderTab::Auth => auth_editor(ui, &mut settings.auth),
            FolderTab::Variables => variable_table(ui, id, &mut settings.variables),
        });

    if settings == folder.settings {
        return None;
    }
    project
        .save_folder(path, settings)
        .err()
        .map(|error| error.to_string())
}
