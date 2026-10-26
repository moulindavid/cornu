use std::path::PathBuf;

use cornu_storage::{History, HistoryEntry, Outcome};
use eframe::egui;

use crate::format;
use crate::response::status_color;
use crate::tree::method_color;

pub enum HistoryAction {
    Open(usize),
    OpenSource(PathBuf),
    Clear,
}

#[derive(Default)]
pub struct HistoryPanel {
    filter: String,
}

impl HistoryPanel {
    /// Indices in actions refer to `History::entries`.
    pub fn show(&mut self, ui: &mut egui::Ui, history: &History) -> Option<HistoryAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            let clear = ui.add_enabled(!history.entries().is_empty(), egui::Button::new("Clear"));
            if clear.clicked() {
                action = Some(HistoryAction::Clear);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter by URL or name")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(4.0);
        if history.entries().is_empty() {
            ui.weak("Requests you send show up here.");
            return action;
        }

        let filter = self.filter.to_lowercase();
        let now = format::unix_now();
        for (index, entry) in history.entries().iter().enumerate().rev() {
            if !filter.is_empty() && !matches(entry, &filter) {
                continue;
            }
            let row = ui.selectable_label(false, row_label(ui, entry, now));
            if row.clicked() {
                action = Some(HistoryAction::Open(index));
            }
            row.on_hover_text(&entry.request.url).context_menu(|ui| {
                if ui.button("Open as draft").clicked() {
                    action = Some(HistoryAction::Open(index));
                }
                if let Some(source) = &entry.source
                    && ui.button("Open saved request").clicked()
                {
                    action = Some(HistoryAction::OpenSource(source.clone()));
                }
            });
        }
        action
    }
}

fn matches(entry: &HistoryEntry, filter: &str) -> bool {
    entry.request.url.to_lowercase().contains(filter)
        || entry.request.name.to_lowercase().contains(filter)
}

fn row_label(ui: &egui::Ui, entry: &HistoryEntry, now: i64) -> egui::text::LayoutJob {
    let small = egui::FontId::monospace(10.0);
    let text = egui::TextStyle::Button.resolve(ui.style());
    let weak = ui.visuals().weak_text_color();
    let request = &entry.request;

    let mut job = egui::text::LayoutJob::default();
    job.append(
        &format!("{} ", request.method),
        0.0,
        egui::TextFormat::simple(small.clone(), method_color(&request.method, ui.visuals())),
    );
    let title = if request.name.is_empty() {
        &request.url
    } else {
        &request.name
    };
    job.append(
        &ellipsize(title, 48),
        0.0,
        egui::TextFormat::simple(text, ui.visuals().text_color()),
    );
    job.append("\n", 0.0, egui::TextFormat::simple(small.clone(), weak));

    let (status, color, details) = match &entry.outcome {
        Outcome::Response {
            status,
            elapsed_ms,
            size,
        } => (
            status.to_string(),
            status_color(*status),
            format!(
                " · {} · {}",
                format::duration(std::time::Duration::from_millis(*elapsed_ms)),
                format::size(*size as usize),
            ),
        ),
        Outcome::Error { message } => (
            "ERR".into(),
            ui.visuals().error_fg_color,
            format!(" · {}", ellipsize(message, 40)),
        ),
    };
    job.append(&status, 0.0, egui::TextFormat::simple(small.clone(), color));
    let ago = format::ago(now - entry.timestamp);
    job.append(
        &format!("{details} · {ago}"),
        0.0,
        egui::TextFormat::simple(small, weak),
    );
    job
}

/// Rows are two lines, so they are shortened by hand instead of wrapped.
fn ellipsize(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_ellipsized_on_char_boundaries() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("éééé", 2), "éé…");
    }

    fn entry(name: &str, url: &str, outcome: Outcome) -> HistoryEntry {
        HistoryEntry {
            timestamp: format::unix_now() - 120,
            request: cornu_core::Request::new(cornu_core::Method::Post, url).with_name(name),
            source: None,
            outcome,
        }
    }

    #[test]
    fn filter_matches_url_or_name() {
        let entry = entry(
            "Create user",
            "{{base_url}}/users",
            Outcome::Error {
                message: "refused".into(),
            },
        );
        assert!(matches(&entry, "users"));
        assert!(matches(&entry, "create"));
        assert!(!matches(&entry, "orders"));
    }

    #[test]
    fn panel_renders_successes_and_errors() {
        let mut history = History::default();
        let ok = Outcome::Response {
            status: 201,
            elapsed_ms: 85,
            size: 2048,
        };
        history.push(entry("Create user", "/users", ok)).unwrap();
        let failed = Outcome::Error {
            message: "connection refused ".repeat(10),
        };
        history.push(entry("", "/health", failed)).unwrap();

        let ctx = egui::Context::default();
        let mut panel = HistoryPanel::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(panel.show(ui, &history).is_none());
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        assert!(
            text.contains("Create user") && text.contains("201"),
            "{text}"
        );
        assert!(text.contains("ERR") && text.contains("2 min ago"), "{text}");
    }
}
