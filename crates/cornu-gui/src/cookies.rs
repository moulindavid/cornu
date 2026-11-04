use cornu_core::CookieJar;
use eframe::egui;

use crate::format;

#[derive(Default)]
pub struct CookiesWindow {
    pub open: bool,
    filter: String,
}

impl CookiesWindow {
    pub fn button(&mut self, ui: &mut egui::Ui, jar: &CookieJar) {
        let count = jar.list().len();
        let label = if count == 0 {
            "🍪".to_owned()
        } else {
            format!("🍪 {count}")
        };
        if ui.button(label).on_hover_text("Cookies").clicked() {
            self.open = true;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, jar: &CookieJar, on_disk: bool) {
        let mut open = self.open;
        egui::Window::new("Cookies")
            .open(&mut open)
            .default_size([760.0, 380.0])
            .collapsible(false)
            .show(ctx, |ui| self.contents(ui, jar, on_disk));
        self.open = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui, jar: &CookieJar, on_disk: bool) {
        let cookies = jar.list();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter by domain or name")
                    .desired_width(240.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!cookies.is_empty(), egui::Button::new("Clear all"))
                    .clicked()
                {
                    jar.clear();
                }
            });
        });
        ui.weak(if on_disk {
            "Set by responses and sent back automatically. Kept in .cornu/cookies.json, outside git."
        } else {
            "Set by responses and sent back automatically. Open a workspace to keep them."
        });
        ui.add_space(4.0);
        if cookies.is_empty() {
            ui.weak("No cookies yet.");
            return;
        }

        let filter = self.filter.trim().to_lowercase();
        let now = format::unix_now();
        let mut remove = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("cookies")
                .num_columns(7)
                .striped(true)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for header in ["Domain", "Path", "Name", "Value", "Expires", "Flags", ""] {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for cookie in cookies.iter().filter(|cookie| {
                        filter.is_empty()
                            || cookie.domain.to_lowercase().contains(&filter)
                            || cookie.name.to_lowercase().contains(&filter)
                    }) {
                        ui.label(&cookie.domain);
                        ui.label(&cookie.path);
                        ui.monospace(&cookie.name);
                        ui.add(egui::Label::new(format::truncate(&cookie.value, 40)))
                            .on_hover_text(&cookie.value);
                        ui.label(match cookie.expires {
                            Some(at) => format::until(at - now),
                            None => "session".into(),
                        });
                        let flags: Vec<&str> =
                            [(cookie.secure, "Secure"), (cookie.http_only, "HttpOnly")]
                                .into_iter()
                                .filter_map(|(set, flag)| set.then_some(flag))
                                .collect();
                        ui.weak(flags.join(" "));
                        if ui.small_button("🗑").on_hover_text("Delete").clicked() {
                            remove = Some(cookie);
                        }
                        ui.end_row();
                    }
                });
        });
        if let Some(cookie) = remove {
            jar.remove(&cookie.domain, &cookie.path, &cookie.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_cookies_and_filters_them() {
        let jar = CookieJar::default();
        let url = url::Url::parse("https://api.example/").unwrap();
        jar.insert("session=abc; HttpOnly; Secure", &url);
        jar.insert(
            "theme=dark",
            &url::Url::parse("https://other.example/").unwrap(),
        );
        let mut window = CookiesWindow {
            open: true,
            ..Default::default()
        };
        let render = |window: &mut CookiesWindow| {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                window.contents(ui, &jar, true)
            });
            output.textures_delta.clear();
            format!("{:?}", output.shapes)
        };
        let shapes = render(&mut window);
        for expected in [
            "api.example",
            "session",
            "HttpOnly",
            "theme",
            "cookies.json",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }
        window.filter = "api".into();
        assert!(!render(&mut window).contains("theme"));
    }
}
