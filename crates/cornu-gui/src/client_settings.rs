use std::path::{Path, PathBuf};
use std::time::Duration;

use cornu_core::{ClientOptions, ProxyMode};
use eframe::egui;

#[derive(Default)]
pub struct ClientSettingsWindow {
    pub open: bool,
    /// The options being edited, until applied.
    draft: Option<ClientOptions>,
    /// Why the last apply failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyKind {
    System,
    None,
    Custom,
}

impl ClientSettingsWindow {
    pub fn open(&mut self) {
        self.open = true;
        self.draft = None;
        self.error = None;
    }

    /// Shown in the top bar while certificates are not verified, which is easy to forget.
    pub fn warning(&mut self, ui: &mut egui::Ui, current: &ClientOptions) {
        if current.insecure
            && ui
                .button(egui::RichText::new("⚠ TLS unverified").color(ui.visuals().warn_fg_color))
                .on_hover_text("Certificates are not checked. Click to change the client settings.")
                .clicked()
        {
            self.open();
        }
    }

    /// Returns the options to apply when the user asks for it.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        current: &ClientOptions,
        root: Option<&Path>,
    ) -> Option<ClientOptions> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut apply = None;
        egui::Window::new("Client settings")
            .open(&mut open)
            .default_width(560.0)
            .collapsible(false)
            .show(ctx, |ui| apply = self.contents(ui, current, root));
        self.open = open;
        apply
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        current: &ClientOptions,
        root: Option<&Path>,
    ) -> Option<ClientOptions> {
        let draft = self.draft.get_or_insert_with(|| current.clone());
        egui::Grid::new("client-settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Timeout");
                ui.horizontal(|ui| {
                    let mut enabled = draft.timeout.is_some();
                    ui.checkbox(&mut enabled, "");
                    let mut seconds = draft.timeout.map_or(30, |timeout| timeout.as_secs());
                    ui.add_enabled(
                        enabled,
                        egui::DragValue::new(&mut seconds)
                            .range(1..=3600)
                            .suffix(" s"),
                    );
                    draft.timeout = enabled.then(|| Duration::from_secs(seconds));
                    if !enabled {
                        ui.weak("waits forever");
                    }
                });
                ui.end_row();

                ui.label("Redirects");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut draft.follow_redirects, "Follow, at most");
                    ui.add_enabled(
                        draft.follow_redirects,
                        egui::DragValue::new(&mut draft.max_redirects).range(1..=50),
                    );
                });
                ui.end_row();

                proxy_rows(ui, &mut draft.proxy);

                ui.label("CA certificate");
                path_field(
                    ui,
                    &mut draft.ca_certificate,
                    "PEM, trusted on top of the system certificates",
                    root,
                );
                ui.end_row();

                ui.label("Client certificate");
                path_field(
                    ui,
                    &mut draft.client_certificate,
                    "PEM with the certificate and its private key",
                    root,
                );
                ui.end_row();

                ui.label("Verification");
                let text = egui::RichText::new("Do not verify certificates (development only)");
                let text = if draft.insecure {
                    text.color(ui.visuals().warn_fg_color)
                } else {
                    text
                };
                ui.checkbox(&mut draft.insecure, text);
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.weak(if root.is_some() {
            "Saved in cornu.toml and shared through git. Relative paths start from the workspace."
        } else {
            "Open a workspace to keep these settings."
        });
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        let mut apply = None;
        ui.horizontal(|ui| {
            let changed = draft != current;
            if ui
                .add_enabled(changed, egui::Button::new("Apply"))
                .clicked()
            {
                apply = Some(draft.clone());
            }
            if ui
                .add_enabled(changed, egui::Button::new("Revert"))
                .clicked()
            {
                *draft = current.clone();
                self.error = None;
            }
            if ui
                .add_enabled(!draft.is_default(), egui::Button::new("Defaults"))
                .clicked()
            {
                *draft = ClientOptions::default();
            }
        });
        apply
    }

    /// The draft becomes the new state once the app could apply it.
    pub fn applied(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.draft = None;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

fn proxy_rows(ui: &mut egui::Ui, proxy: &mut ProxyMode) {
    let kind = match proxy {
        ProxyMode::System => ProxyKind::System,
        ProxyMode::None => ProxyKind::None,
        ProxyMode::Custom { .. } => ProxyKind::Custom,
    };
    ui.label("Proxy");
    ui.horizontal(|ui| {
        let mut selected = kind;
        ui.radio_value(&mut selected, ProxyKind::System, "System")
            .on_hover_text("From HTTP_PROXY, HTTPS_PROXY and NO_PROXY");
        ui.radio_value(&mut selected, ProxyKind::None, "None");
        ui.radio_value(&mut selected, ProxyKind::Custom, "Custom");
        if selected != kind {
            *proxy = match selected {
                ProxyKind::System => ProxyMode::System,
                ProxyKind::None => ProxyMode::None,
                ProxyKind::Custom => ProxyMode::Custom {
                    url: String::new(),
                    no_proxy: String::new(),
                },
            };
        }
    });
    ui.end_row();
    if let ProxyMode::Custom { url, no_proxy } = proxy {
        ui.label("");
        ui.add(
            egui::TextEdit::singleline(url)
                .hint_text("http://user:password@proxy:3128 or socks5://localhost:1080")
                .desired_width(380.0),
        );
        ui.end_row();
        ui.label("");
        ui.add(
            egui::TextEdit::singleline(no_proxy)
                .hint_text("Bypassed: localhost, .internal, 10.0.0.0/8")
                .desired_width(380.0),
        );
        ui.end_row();
    }
}

fn path_field(ui: &mut egui::Ui, path: &mut Option<PathBuf>, hint: &str, root: Option<&Path>) {
    ui.horizontal(|ui| {
        let mut text = path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let edit = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(hint)
                .desired_width(300.0),
        );
        if edit.changed() {
            *path = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
        }
        if ui.button("Browse…").clicked()
            && let Some(picked) = rfd::FileDialog::new()
                .add_filter("PEM", &["pem", "crt", "cer", "key"])
                .pick_file()
        {
            *path = Some(relative(picked, root));
        }
        if path.is_some() && ui.small_button("×").on_hover_text("Remove").clicked() {
            *path = None;
        }
    });
}

/// Inside the workspace, kept relative so the setting works on every clone.
fn relative(path: PathBuf, root: Option<&Path>) -> PathBuf {
    root.and_then(|root| path.strip_prefix(root).ok())
        .map(Path::to_path_buf)
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(window: &mut ClientSettingsWindow, current: &ClientOptions) -> String {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        // A window is only sized on its first frame and drawn from the second.
        let mut output = ctx.run_ui(input.clone(), |ui| {
            window.show(ui.ctx(), current, Some(Path::new("/work")));
        });
        if window.open {
            output.textures_delta.clear();
            output = ctx.run_ui(input, |ui| {
                window.show(ui.ctx(), current, Some(Path::new("/work")));
            });
        }
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    #[test]
    fn window_shows_the_current_options() {
        let mut window = ClientSettingsWindow::default();
        assert_eq!(render(&mut window, &ClientOptions::default()), "[]");

        window.open();
        let current = ClientOptions {
            proxy: ProxyMode::Custom {
                url: "http://proxy:3128".into(),
                no_proxy: String::new(),
            },
            client_certificate: Some("certs/client.pem".into()),
            insecure: true,
            ..Default::default()
        };
        let shapes = render(&mut window, &current);
        for expected in [
            "Client settings",
            "Timeout",
            "http://proxy:3128",
            "Bypassed",
            "certs/client.pem",
            "Do not verify",
            "cornu.toml",
            "Apply",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }
    }

    #[test]
    fn picked_files_inside_the_workspace_stay_relative() {
        let root = Path::new("/work");
        assert_eq!(
            relative("/work/certs/ca.pem".into(), Some(root)),
            Path::new("certs/ca.pem")
        );
        assert_eq!(
            relative("/etc/ssl/ca.pem".into(), Some(root)),
            Path::new("/etc/ssl/ca.pem")
        );
        assert_eq!(
            relative("/work/ca.pem".into(), None),
            Path::new("/work/ca.pem")
        );
    }

    #[test]
    fn failed_apply_keeps_the_draft_and_shows_why() {
        let mut window = ClientSettingsWindow::default();
        window.open();
        let current = ClientOptions::default();
        render(&mut window, &current);
        window.draft.as_mut().unwrap().insecure = true;
        window.applied(Err("invalid certificate".into()));
        let shapes = render(&mut window, &current);
        assert!(shapes.contains("invalid certificate"), "{shapes}");
        assert!(window.draft.as_ref().unwrap().insecure);
        window.applied(Ok(()));
        assert!(window.draft.is_none() && window.error.is_none());
    }
}
