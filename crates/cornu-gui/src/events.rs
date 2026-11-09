use std::time::Duration;

use cornu_core::SseEvent;
use eframe::egui;

use crate::format;

/// The events of a server-sent event stream, while it streams and once it ended.
#[derive(Default)]
pub struct EventLog {
    /// With the time since the request was sent.
    entries: Vec<(Duration, SseEvent)>,
    filter: String,
}

impl EventLog {
    pub fn push(&mut self, at: Duration, event: SseEvent) {
        self.entries.push((at, event));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter events")
                    .desired_width(200.0),
            );
            ui.weak(match self.entries.len() {
                1 => "1 event".to_owned(),
                count => format!("{count} events"),
            });
            if !self.entries.is_empty() && ui.small_button("Clear").clicked() {
                self.entries.clear();
            }
        });
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("event-log")
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.entries.is_empty() {
                    ui.weak("No events yet.");
                }
                for (at, event) in self.entries.iter().filter(|(_, event)| {
                    filter.is_empty()
                        || event.event.to_lowercase().contains(&filter)
                        || event.data.to_lowercase().contains(&filter)
                }) {
                    event_row(ui, *at, event);
                }
            });
    }
}

fn event_row(ui: &mut egui::Ui, at: Duration, event: &SseEvent) {
    ui.horizontal_top(|ui| {
        ui.weak(format!("{:>8.3}s", at.as_secs_f64()));
        if ui.small_button("📋").on_hover_text("Copy data").clicked() {
            ui.ctx().copy_text(event.data.clone());
        }
        if event.event != "message" {
            ui.label(
                egui::RichText::new(&event.event)
                    .monospace()
                    .strong()
                    .color(egui::Color32::from_rgb(0x50, 0xa0, 0xf0)),
            );
        }
        if let Some(id) = &event.id {
            ui.weak(format!("#{id}"));
        }
        let shown = format::pretty_json(&event.data).unwrap_or_else(|| event.data.clone());
        ui.add(egui::Label::new(egui::RichText::new(shown).monospace()).wrap());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, data: &str, id: Option<&str>) -> SseEvent {
        SseEvent {
            event: name.into(),
            data: data.into(),
            id: id.map(Into::into),
            retry: None,
        }
    }

    fn render(log: &mut EventLog) -> String {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| log.show(ui));
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    #[test]
    fn events_are_listed_and_filtered() {
        let mut log = EventLog::default();
        assert!(render(&mut log).contains("No events yet."));

        log.push(Duration::from_millis(120), event("message", "hello", None));
        log.push(
            Duration::from_millis(1500),
            event("price", "{\"btc\":1}", Some("42")),
        );
        let shapes = render(&mut log);
        for expected in [
            "2 events",
            "hello",
            "price",
            "#42",
            "\\\"btc\\\": 1",
            "1.500s",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }
        assert!(!shapes.contains("\"message\""), "{shapes}");

        log.filter = "PRICE".into();
        let shapes = render(&mut log);
        assert!(
            shapes.contains("btc") && !shapes.contains("hello"),
            "{shapes}"
        );
    }
}
