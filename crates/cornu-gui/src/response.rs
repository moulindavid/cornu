use std::sync::Arc;

use cornu_core::Response;
use eframe::egui::{self, Color32, FontId, Galley, RichText};

use crate::format;
use crate::highlight::{self, Kind, Palette};

const HIGHLIGHT_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Body,
    Headers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pretty,
    Raw,
}

#[derive(PartialEq)]
struct CacheKey {
    mode: Mode,
    search: String,
    dark_mode: bool,
    font_size: f32,
}

struct Rendered {
    key: CacheKey,
    galley: Arc<Galley>,
    matches: usize,
}

pub struct ResponseView {
    response: Response,
    raw: Option<String>,
    pretty: Option<String>,
    tokens: Vec<(std::ops::Range<usize>, Kind)>,
    tab: Tab,
    mode: Mode,
    search: String,
    rendered: Option<Rendered>,
}

impl ResponseView {
    pub fn new(response: Response) -> Self {
        let raw = String::from_utf8(response.body.clone()).ok();
        let pretty = raw.as_deref().and_then(pretty_json);
        let tokens = match &pretty {
            Some(pretty) if pretty.len() <= HIGHLIGHT_LIMIT => highlight::json_tokens(pretty),
            _ => Vec::new(),
        };
        let mode = if pretty.is_some() {
            Mode::Pretty
        } else {
            Mode::Raw
        };
        Self {
            response,
            raw,
            pretty,
            tokens,
            tab: Tab::Body,
            mode,
            search: String::new(),
            rendered: None,
        }
    }

    fn body_text(&self) -> Option<&str> {
        match self.mode {
            Mode::Pretty => self.pretty.as_deref().or(self.raw.as_deref()),
            Mode::Raw => self.raw.as_deref(),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let response = &self.response;
        ui.horizontal(|ui| {
            let status = format!("{} {}", response.status, response.reason().unwrap_or(""));
            ui.label(
                RichText::new(status)
                    .strong()
                    .color(status_color(response.status)),
            );
            ui.separator();
            ui.label(format::duration(response.elapsed));
            ui.separator();
            ui.label(format::size(response.size()));
            ui.separator();
            ui.selectable_value(&mut self.tab, Tab::Body, "Body");
            let headers = format!("Headers ({})", response.headers.len());
            ui.selectable_value(&mut self.tab, Tab::Headers, headers);
        });
        ui.separator();
        match self.tab {
            Tab::Body => self.body(ui),
            Tab::Headers => headers_table(ui, &self.response.headers),
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        if self.raw.is_none() {
            self.binary_body(ui);
            return;
        }

        ui.horizontal(|ui| {
            ui.add_enabled_ui(self.pretty.is_some(), |ui| {
                ui.selectable_value(&mut self.mode, Mode::Pretty, "Pretty")
                    .on_disabled_hover_text("The body is not valid JSON");
            });
            ui.selectable_value(&mut self.mode, Mode::Raw, "Raw");
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("🔍 Search")
                    .desired_width(200.0),
            );
            if let Some(rendered) = &self.rendered
                && !self.search.is_empty()
            {
                ui.weak(match rendered.matches {
                    1 => "1 match".to_owned(),
                    count => format!("{count} matches"),
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("💾 Save").clicked() {
                    save_body(&self.response.body);
                }
                if ui.button("📋 Copy").clicked()
                    && let Some(text) = self.body_text()
                {
                    ui.ctx().copy_text(text.to_owned());
                }
            });
        });
        ui.add_space(4.0);

        let galley = self.galley(ui);
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.add(egui::Label::new(galley).selectable(true).extend());
        });
    }

    fn galley(&mut self, ui: &egui::Ui) -> Arc<Galley> {
        let font = FontId::monospace(ui.style().text_styles[&egui::TextStyle::Monospace].size);
        let key = CacheKey {
            mode: self.mode,
            search: self.search.clone(),
            dark_mode: ui.visuals().dark_mode,
            font_size: font.size,
        };
        if let Some(rendered) = &self.rendered
            && rendered.key == key
        {
            return rendered.galley.clone();
        }

        let text = self.body_text().unwrap_or_default();
        let use_tokens = self.mode == Mode::Pretty && self.pretty.is_some();
        let tokens = if use_tokens { &self.tokens[..] } else { &[] };
        let matches = highlight::find_matches(text, &self.search);
        let job = highlight::layout(text, tokens, &matches, &Palette::new(ui.visuals()), font);
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        self.rendered = Some(Rendered {
            key,
            galley: galley.clone(),
            matches: matches.len(),
        });
        galley
    }

    fn binary_body(&self, ui: &mut egui::Ui) {
        let content_type = self.response.content_type().unwrap_or("unknown type");
        ui.label(format!(
            "Binary content ({content_type}, {}).",
            format::size(self.response.size())
        ));
        if ui.button("💾 Save to file…").clicked() {
            save_body(&self.response.body);
        }
    }
}

fn pretty_json(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with(['{', '[']) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

fn headers_table(ui: &mut egui::Ui, headers: &[(String, String)]) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new("response-headers")
                .num_columns(2)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for (name, value) in headers {
                        ui.add(egui::Label::new(RichText::new(name).strong()).selectable(true));
                        ui.add(egui::Label::new(value).selectable(true));
                        ui.end_row();
                    }
                });
        });
}

fn save_body(body: &[u8]) {
    if let Some(path) = rfd::FileDialog::new().save_file()
        && let Err(error) = std::fs::write(&path, body)
    {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Could not save response")
            .set_description(format!("{}: {error}", path.display()))
            .show();
    }
}

fn status_color(status: u16) -> Color32 {
    match status {
        200..300 => Color32::from_rgb(0x2e, 0xa0, 0x43),
        300..400 => Color32::from_rgb(0x3b, 0x82, 0xf6),
        400..500 => Color32::from_rgb(0xd9, 0x77, 0x06),
        _ => Color32::from_rgb(0xdc, 0x26, 0x26),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn response(body: &[u8]) -> Response {
        Response {
            status: 200,
            headers: Vec::new(),
            body: body.to_vec(),
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn json_bodies_open_in_pretty_mode() {
        let view = ResponseView::new(response(br#"{"a":1}"#));
        assert_eq!(view.mode, Mode::Pretty);
        assert_eq!(view.body_text(), Some("{\n  \"a\": 1\n}"));
        assert!(!view.tokens.is_empty());
    }

    #[test]
    fn pretty_json_keeps_server_key_order() {
        assert_eq!(
            pretty_json(r#"{"z":1,"a":2}"#).as_deref(),
            Some("{\n  \"z\": 1,\n  \"a\": 2\n}")
        );
    }

    #[test]
    fn non_json_bodies_open_in_raw_mode() {
        let view = ResponseView::new(response(b"<html></html>"));
        assert_eq!(view.mode, Mode::Raw);
        assert!(view.pretty.is_none());
        assert_eq!(pretty_json("42"), None);
    }

    #[test]
    fn invalid_utf8_is_treated_as_binary() {
        let view = ResponseView::new(response(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(view.raw.is_none());
        assert_eq!(view.body_text(), None);
    }
}
