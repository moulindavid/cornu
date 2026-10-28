use std::sync::mpsc::Receiver;

use cornu_core::{Method, Request, Response, SendError};
use cornu_import::Collection;
use eframe::egui;

use crate::worker::Worker;

/// The "Import OpenAPI from URL" dialog and its download.
#[derive(Default)]
pub struct SpecImport {
    open: bool,
    url: String,
    error: Option<String>,
    pending: Option<Receiver<Result<Response, SendError>>>,
}

impl SpecImport {
    pub fn open(&mut self) {
        self.open = true;
        self.error = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, worker: &Worker) -> Option<Collection> {
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
            ui.label("URL of a JSON or YAML spec:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.url)
                    .hint_text("https://petstore3.swagger.io/api/v3/openapi.json")
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
                    ui.weak("Downloading…");
                }
            });
        });
        if start && self.pending.is_none() && !self.url.trim().is_empty() {
            self.error = None;
            let request = Request::new(Method::Get, self.url.trim()).with_header(
                "Accept",
                "application/json, application/yaml;q=0.9, */*;q=0.8",
            );
            self.pending = Some(worker.fetch(ctx, request));
        }
        if cancel || modal.should_close() {
            self.open = false;
            self.pending = None;
        }
        None
    }

    fn poll(&mut self) -> Option<Collection> {
        let result = self.pending.as_ref()?.try_recv().ok()?;
        self.pending = None;
        match parse_download(result, self.url.trim()) {
            Ok(collection) => Some(collection),
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }
}

fn parse_download(result: Result<Response, SendError>, url: &str) -> Result<Collection, String> {
    let response = result.map_err(|error| error.to_string())?;
    if !(200..300).contains(&response.status) {
        return Err(format!("The server answered {}", response.status));
    }
    let text = String::from_utf8_lossy(&response.body);
    cornu_import::openapi::parse_spec(&text, Some(url)).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            elapsed: Default::default(),
        }
    }

    #[test]
    fn downloads_are_parsed_relative_to_their_url() {
        let spec = "openapi: 3.0.0\ninfo:\n  title: Remote\nservers:\n  - url: /v1\npaths: {}\n";
        let collection = parse_download(
            Ok(response(200, spec)),
            "https://api.example/docs/openapi.yaml",
        )
        .unwrap();
        assert_eq!(collection.root.name, "Remote");
        assert_eq!(collection.root.variables[0].value, "https://api.example/v1");
    }

    #[test]
    fn http_errors_and_bad_documents_are_reported() {
        let error = parse_download(Ok(response(404, "nope")), "https://x").unwrap_err();
        assert!(error.contains("404"), "{error}");
        let error = parse_download(Ok(response(200, "<html>")), "https://x").unwrap_err();
        assert!(!error.is_empty());
    }
}
