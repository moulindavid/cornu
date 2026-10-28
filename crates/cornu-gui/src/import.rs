use std::sync::mpsc::Receiver;

use cornu_import::discovery::{Discovered, DiscoveryError, discover};
use eframe::egui;

use crate::worker::Worker;

/// The "Import OpenAPI from URL" dialog and its background spec discovery.
#[derive(Default)]
pub struct SpecImport {
    open: bool,
    url: String,
    error: Option<String>,
    pending: Option<Receiver<Result<Discovered, DiscoveryError>>>,
}

impl SpecImport {
    pub fn open(&mut self) {
        self.open = true;
        self.error = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, worker: &Worker) -> Option<Discovered> {
        let imported = self.poll();
        if imported.is_some() {
            self.open = false;
            return imported;
        }
        if !self.open {
            return None;
        }

        let mut start = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("import-spec")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading("Import OpenAPI / Swagger");
            ui.label("URL of the spec, of the API or of its docs page:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text("https://petstore3.swagger.io")
                    .desired_width(f32::INFINITY),
            );
            if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                start = true;
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let downloading = self.pending.is_some();
                let ready = !downloading && !self.url.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new("Import")).clicked() {
                    start = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if downloading {
                    ui.add(egui::Spinner::new());
                    ui.weak("Looking for a spec…");
                }
            });
        });
        if start && self.pending.is_none() && !self.url.trim().is_empty() {
            self.error = None;
            let url = self.url.trim().to_owned();
            self.pending =
                Some(worker.run(ctx, |client| async move { discover(&client, &url).await }));
        }
        if cancel || modal.should_close() {
            self.open = false;
            self.pending = None;
        }
        None
    }

    fn poll(&mut self) -> Option<Discovered> {
        let result = self.pending.as_ref()?.try_recv().ok()?;
        self.pending = None;
        match result {
            Ok(discovered) => Some(discovered),
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }
}
