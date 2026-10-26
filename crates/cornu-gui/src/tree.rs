use std::path::{Path, PathBuf};

use cornu_core::Method;
use cornu_storage::{Folder, Item, RequestEntry};
use eframe::egui::{self, collapsing_header::CollapsingState};

pub enum TreeAction {
    OpenRequest(PathBuf),
    OpenFolder(PathBuf),
    NewRequest(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf, String),
    Duplicate(PathBuf),
    Delete(PathBuf),
}

struct Renaming {
    path: PathBuf,
    name: String,
    focus: bool,
}

#[derive(Default)]
pub struct Tree {
    renaming: Option<Renaming>,
}

impl Tree {
    pub fn start_rename(&mut self, path: PathBuf, name: &str) {
        self.renaming = Some(Renaming {
            path,
            name: name.to_owned(),
            focus: true,
        });
    }

    pub fn expand(ctx: &egui::Context, path: &Path) {
        for ancestor in path.ancestors() {
            let mut state =
                CollapsingState::load_with_default_open(ctx, folder_id(ancestor), false);
            state.set_open(true);
            state.store(ctx);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        root: &Folder,
        active: Option<&Path>,
    ) -> Vec<TreeAction> {
        let mut actions = Vec::new();
        if root.items.is_empty() {
            ui.weak("Empty workspace. Add a request or a folder.");
        }
        for item in &root.items {
            self.item(ui, item, active, &mut actions);
        }
        actions
    }

    fn item(
        &mut self,
        ui: &mut egui::Ui,
        item: &Item,
        active: Option<&Path>,
        actions: &mut Vec<TreeAction>,
    ) {
        match item {
            Item::Folder(folder) => self.folder(ui, folder, active, actions),
            Item::Request(entry) => {
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    self.request(ui, entry, active, actions);
                });
            }
        }
    }

    fn folder(
        &mut self,
        ui: &mut egui::Ui,
        folder: &Folder,
        active: Option<&Path>,
        actions: &mut Vec<TreeAction>,
    ) {
        let id = folder_id(&folder.path);
        let mut toggle = false;
        CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| {
                if self.rename_field(ui, &folder.path, actions) {
                    return;
                }
                let selected = active == Some(folder.path.as_path());
                let label = ui.selectable_label(selected, format!("📁 {}", folder.settings.name));
                if label.clicked() {
                    toggle = true;
                    actions.push(TreeAction::OpenFolder(folder.path.clone()));
                }
                label.context_menu(|ui| {
                    if ui.button("New request").clicked() {
                        actions.push(TreeAction::NewRequest(folder.path.clone()));
                    }
                    if ui.button("New folder").clicked() {
                        actions.push(TreeAction::NewFolder(folder.path.clone()));
                    }
                    if ui.button("Settings").clicked() {
                        actions.push(TreeAction::OpenFolder(folder.path.clone()));
                    }
                    ui.separator();
                    self.common_menu(ui, &folder.path, &folder.settings.name, actions);
                });
            })
            .body(|ui| {
                if folder.items.is_empty() {
                    ui.weak("Empty");
                }
                for child in &folder.items {
                    self.item(ui, child, active, actions);
                }
            });
        if toggle {
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
            state.toggle(ui);
            state.store(ui.ctx());
        }
    }

    fn request(
        &mut self,
        ui: &mut egui::Ui,
        entry: &RequestEntry,
        active: Option<&Path>,
        actions: &mut Vec<TreeAction>,
    ) {
        if self.rename_field(ui, &entry.path, actions) {
            return;
        }
        let selected = active == Some(entry.path.as_path());
        let label = ui.selectable_label(selected, request_label(ui, entry));
        if label.clicked() {
            actions.push(TreeAction::OpenRequest(entry.path.clone()));
        }
        label.context_menu(|ui| {
            if ui.button("Open").clicked() {
                actions.push(TreeAction::OpenRequest(entry.path.clone()));
            }
            ui.separator();
            self.common_menu(ui, &entry.path, &entry.request.name, actions);
        });
    }

    fn common_menu(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        name: &str,
        actions: &mut Vec<TreeAction>,
    ) {
        if ui.button("Rename").clicked() {
            self.start_rename(path.to_owned(), name);
        }
        if ui.button("Duplicate").clicked() {
            actions.push(TreeAction::Duplicate(path.to_owned()));
        }
        if ui.button("Delete").clicked() {
            actions.push(TreeAction::Delete(path.to_owned()));
        }
    }

    /// Shows the inline rename field if `path` is being renamed.
    fn rename_field(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        actions: &mut Vec<TreeAction>,
    ) -> bool {
        let Some(renaming) = self.renaming.as_mut().filter(|r| r.path == path) else {
            return false;
        };
        let id = egui::Id::unique(("rename", path));
        let mut output = egui::TextEdit::singleline(&mut renaming.name)
            .id(id)
            .desired_width(ui.available_width())
            .show(ui);
        if renaming.focus {
            renaming.focus = false;
            output.response.request_focus();
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(renaming.name.chars().count()),
            );
            output.state.cursor.set_char_range(Some(all));
            output.state.store(ui.ctx(), id);
        } else if output.response.lost_focus() {
            let cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
            let name = renaming.name.trim();
            if !cancelled && !name.is_empty() {
                actions.push(TreeAction::Rename(path.to_owned(), name.to_owned()));
            }
            self.renaming = None;
        }
        true
    }
}

fn folder_id(path: &Path) -> egui::Id {
    egui::Id::unique(("tree-folder", path))
}

fn request_label(ui: &egui::Ui, entry: &RequestEntry) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let method = &entry.request.method;
    job.append(
        &format!("{:<4} ", short_method(method)),
        0.0,
        egui::TextFormat::simple(
            egui::FontId::monospace(10.0),
            method_color(method, ui.visuals()),
        ),
    );
    let name = if entry.request.name.is_empty() {
        "Untitled request"
    } else {
        &entry.request.name
    };
    job.append(
        name,
        0.0,
        egui::TextFormat::simple(
            egui::TextStyle::Button.resolve(ui.style()),
            ui.visuals().text_color(),
        ),
    );
    job
}

fn short_method(method: &Method) -> &str {
    match method {
        Method::Delete => "DEL",
        Method::Options => "OPT",
        Method::Patch => "PTCH",
        other => other.as_str(),
    }
}

pub fn method_color(method: &Method, visuals: &egui::Visuals) -> egui::Color32 {
    let (dark, light) = match method {
        Method::Get => ((0x8c, 0xd0, 0x8c), (0x2e, 0x7d, 0x32)),
        Method::Post => ((0xf0, 0xc0, 0x6c), (0xb2, 0x6a, 0x00)),
        Method::Put => ((0x7d, 0xaf, 0xf0), (0x1e, 0x5a, 0xc8)),
        Method::Patch => ((0xc8, 0x9c, 0xf0), (0x7b, 0x3f, 0xb0)),
        Method::Delete => ((0xf3, 0x8b, 0x8b), (0xc6, 0x28, 0x28)),
        _ => ((0xa0, 0xa0, 0xa0), (0x60, 0x60, 0x60)),
    };
    let (r, g, b) = if visuals.dark_mode { dark } else { light };
    egui::Color32::from_rgb(r, g, b)
}
