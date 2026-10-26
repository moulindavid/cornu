use std::path::Path;

use eframe::egui;

use crate::project::Project;
use crate::widgets::variable_table;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Selection {
    Globals,
    Environment(usize),
}

pub struct Environments {
    pub active: Option<usize>,
    manager_open: bool,
    selected: Selection,
}

impl Default for Environments {
    fn default() -> Self {
        Self {
            active: None,
            manager_open: false,
            selected: Selection::Globals,
        }
    }
}

impl Environments {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn selector(&mut self, ui: &mut egui::Ui, project: &Project) {
        if ui
            .button("⚙")
            .on_hover_text("Manage environments")
            .clicked()
        {
            self.manager_open = true;
        }
        let selected = project
            .environment(self.active)
            .map_or("No environment", |env| env.name.as_str());
        egui::ComboBox::from_id_salt("active-environment")
            .selected_text(selected)
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.active, None, "No environment");
                for (index, entry) in project.workspace.environments.iter().enumerate() {
                    ui.selectable_value(&mut self.active, Some(index), &entry.environment.name);
                }
            });
    }

    /// Edits are saved as they happen; returns an error message if saving failed.
    pub fn manager(&mut self, ctx: &egui::Context, project: &mut Project) -> Option<String> {
        let mut open = self.manager_open;
        let mut error = None;
        egui::Window::new("Environments")
            .open(&mut open)
            .default_size([760.0, 420.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(180.0);
                        error = self.list_column(ui, project);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        if let Some(message) = self.detail_column(ui, project) {
                            error = Some(message);
                        }
                    });
                });
            });
        self.manager_open = open;
        error
    }

    fn list_column(&mut self, ui: &mut egui::Ui, project: &mut Project) -> Option<String> {
        ui.selectable_value(&mut self.selected, Selection::Globals, "🌐 Globals");
        ui.separator();
        for (index, entry) in project.workspace.environments.iter().enumerate() {
            let name = &entry.environment.name;
            let label = if self.active == Some(index) {
                format!("● {name}")
            } else {
                name.clone()
            };
            ui.selectable_value(&mut self.selected, Selection::Environment(index), label);
        }
        ui.add_space(6.0);
        if ui.button("+ New environment").clicked() {
            let name = format!("Environment {}", project.workspace.environments.len() + 1);
            match project.add_environment(&name) {
                Ok(index) => self.selected = Selection::Environment(index),
                Err(error) => return Some(error.to_string()),
            }
        }
        None
    }

    fn detail_column(&mut self, ui: &mut egui::Ui, project: &mut Project) -> Option<String> {
        let id = egui::Id::unique(("environment-variables", self.selected));
        match self.selected {
            Selection::Globals => {
                ui.weak("Global variables apply to every request, whatever the environment.");
                ui.add_space(4.0);
                let mut settings = project.workspace.tree.settings.clone();
                egui::ScrollArea::vertical()
                    .id_salt(id)
                    .show(ui, |ui| variable_table(ui, id, &mut settings.variables));
                if settings != project.workspace.tree.settings {
                    return project
                        .save_folder(Path::new(""), settings)
                        .err()
                        .map(|error| error.to_string());
                }
                None
            }
            Selection::Environment(index) => {
                let Some(entry) = project.workspace.environments.get_mut(index) else {
                    self.selected = Selection::Globals;
                    return None;
                };
                let before = entry.environment.clone();
                let mut delete = false;
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut entry.environment.name);
                    if ui.button("Use").clicked() {
                        self.active = Some(index);
                    }
                    if ui.button("🗑 Delete").clicked() {
                        delete = true;
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt(id).show(ui, |ui| {
                    variable_table(ui, id, &mut entry.environment.variables)
                });
                let result = if delete {
                    self.remove(project, index)
                } else if entry.environment != before {
                    project.save_environment(index)
                } else {
                    Ok(())
                };
                result.err().map(|error| error.to_string())
            }
        }
    }

    fn remove(
        &mut self,
        project: &mut Project,
        index: usize,
    ) -> Result<(), cornu_storage::StorageError> {
        project.remove_environment(index)?;
        self.selected = Selection::Globals;
        self.active = match self.active {
            Some(active) if active == index => None,
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use cornu_core::KeyValue;

    use super::*;

    fn project() -> Project {
        let mut project = Project::scratch();
        project.workspace.tree.settings.variables = vec![
            KeyValue::new("host", "global.example"),
            KeyValue::new("version", "v1"),
        ];
        for (name, host) in [("dev", "dev.example"), ("prod", "prod.example")] {
            let index = project.add_environment(name).unwrap();
            project.workspace.environments[index]
                .environment
                .variables
                .push(KeyValue::new("host", host));
        }
        project
    }

    #[test]
    fn active_environment_shadows_globals() {
        let project = project();
        let mut environments = Environments::default();
        let variables = project.variables(None, environments.active);
        assert_eq!(variables.get("host"), Some("global.example"));
        environments.active = Some(1);
        let variables = project.variables(None, environments.active);
        assert_eq!(variables.get("host"), Some("prod.example"));
        assert_eq!(variables.get("version"), Some("v1"));
    }

    #[test]
    fn removing_keeps_the_active_environment_pointing_at_the_same_one() {
        let mut project = project();
        let mut environments = Environments {
            active: Some(1),
            ..Default::default()
        };
        environments.remove(&mut project, 0).unwrap();
        let variables = project.variables(None, environments.active);
        assert_eq!(variables.get("host"), Some("prod.example"));

        environments.remove(&mut project, 0).unwrap();
        assert_eq!(environments.active, None);
    }

    #[test]
    fn manager_window_does_not_grow_every_frame() {
        let ctx = egui::Context::default();
        let mut project = project();
        let mut environments = Environments {
            manager_open: true,
            ..Default::default()
        };
        let mut width = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                environments.manager(ui.ctx(), &mut project);
            });
            output.textures_delta.clear();
            ctx.memory(|memory| {
                let areas = memory.areas();
                let window = areas
                    .visible_layer_ids()
                    .into_iter()
                    .find(|layer| layer.order == egui::Order::Middle);
                window
                    .and_then(|layer| memory.area_rect(layer.id))
                    .map(|rect| rect.width())
            })
        };
        let settled: Vec<_> = (0..20).map(|_| width()).skip(5).collect();
        assert!(settled[0].is_some());
        assert!(
            settled.windows(2).all(|pair| pair[0] == pair[1]),
            "{settled:?}"
        );
    }
}
