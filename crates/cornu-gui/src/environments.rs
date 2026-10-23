use cornu_core::{Environment, Variables};
use eframe::egui;

use crate::widgets::key_value_table;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Selection {
    Globals,
    Environment(usize),
}

pub struct Environments {
    globals: Environment,
    list: Vec<Environment>,
    active: Option<usize>,
    manager_open: bool,
    selected: Selection,
}

impl Default for Environments {
    fn default() -> Self {
        Self {
            globals: Environment::new("Globals"),
            list: Vec::new(),
            active: None,
            manager_open: false,
            selected: Selection::Globals,
        }
    }
}

impl Environments {
    pub fn variables(&self) -> Variables {
        let mut variables = Variables::new().with_layer(self.globals.enabled());
        if let Some(env) = self.active.and_then(|index| self.list.get(index)) {
            variables.push_layer(env.enabled());
        }
        variables
    }

    pub fn selector(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("⚙")
            .on_hover_text("Manage environments")
            .clicked()
        {
            self.manager_open = true;
        }
        let selected = self
            .active
            .and_then(|index| self.list.get(index))
            .map_or("No environment", |env| env.name.as_str());
        egui::ComboBox::from_id_salt("active-environment")
            .selected_text(selected)
            .width(180.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.active, None, "No environment");
                for (index, env) in self.list.iter().enumerate() {
                    ui.selectable_value(&mut self.active, Some(index), &env.name);
                }
            });
    }

    pub fn manager(&mut self, ctx: &egui::Context) {
        let mut open = self.manager_open;
        egui::Window::new("Environments")
            .open(&mut open)
            .default_size([720.0, 420.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(180.0);
                        self.list_column(ui);
                    });
                    ui.separator();
                    ui.vertical(|ui| self.detail_column(ui));
                });
            });
        self.manager_open = open;
    }

    fn list_column(&mut self, ui: &mut egui::Ui) {
        ui.selectable_value(&mut self.selected, Selection::Globals, "🌐 Globals");
        ui.separator();
        for (index, env) in self.list.iter().enumerate() {
            let label = if self.active == Some(index) {
                format!("● {}", env.name)
            } else {
                env.name.clone()
            };
            ui.selectable_value(&mut self.selected, Selection::Environment(index), label);
        }
        ui.add_space(6.0);
        if ui.button("+ New environment").clicked() {
            self.list.push(Environment::new(format!(
                "Environment {}",
                self.list.len() + 1
            )));
            self.selected = Selection::Environment(self.list.len() - 1);
        }
    }

    fn detail_column(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::unique(("environment-variables", self.selected));
        let mut delete = None;
        let env = match self.selected {
            Selection::Globals => {
                ui.weak("Global variables apply to every request, whatever the environment.");
                &mut self.globals
            }
            Selection::Environment(index) => {
                let Some(env) = self.list.get_mut(index) else {
                    self.selected = Selection::Globals;
                    return;
                };
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut env.name);
                    if ui.button("Use").clicked() {
                        self.active = Some(index);
                    }
                    if ui.button("🗑 Delete").clicked() {
                        delete = Some(index);
                    }
                });
                env
            }
        };
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt(id)
            .show(ui, |ui| key_value_table(ui, id, &mut env.variables));

        if let Some(index) = delete {
            self.remove(index);
        }
    }

    fn remove(&mut self, index: usize) {
        self.list.remove(index);
        self.selected = Selection::Globals;
        self.active = match self.active {
            Some(active) if active == index => None,
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environments() -> Environments {
        let mut environments = Environments {
            globals: Environment::new("Globals")
                .with_variable("host", "global.example")
                .with_variable("version", "v1"),
            ..Default::default()
        };
        environments
            .list
            .push(Environment::new("dev").with_variable("host", "dev.example"));
        environments
            .list
            .push(Environment::new("prod").with_variable("host", "prod.example"));
        environments
    }

    #[test]
    fn active_environment_shadows_globals() {
        let mut environments = environments();
        assert_eq!(environments.variables().get("host"), Some("global.example"));
        environments.active = Some(1);
        let variables = environments.variables();
        assert_eq!(variables.get("host"), Some("prod.example"));
        assert_eq!(variables.get("version"), Some("v1"));
    }

    #[test]
    fn removing_keeps_the_active_environment_pointing_at_the_same_one() {
        let mut environments = environments();
        environments.active = Some(1);
        environments.remove(0);
        assert_eq!(environments.variables().get("host"), Some("prod.example"));

        environments.remove(0);
        assert_eq!(environments.active, None);
    }
}
