use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use cornu_core::{Auth, Grant, OAuth2, SendError, Token, TokenStore, Variables};
use eframe::egui;
use tokio::task::AbortHandle;

use crate::format;
use crate::worker::Worker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAction {
    Get,
    Cancel,
    Clear,
}

/// The token of the OAuth2 configuration shown in an auth editor.
#[derive(Debug, Default)]
pub struct TokenPanel {
    /// With its variables resolved; `None` when the edited auth is not OAuth2.
    pub config: Option<OAuth2>,
    pub token: Option<Token>,
    pub pending: bool,
    /// The authorization page, once the flow has one.
    pub link: Option<String>,
    pub action: Option<TokenAction>,
}

impl TokenPanel {
    pub fn new(
        auth: &Auth,
        variables: &Variables,
        tokens: &TokenStore,
        flow: Option<&Flow>,
    ) -> Self {
        let Auth::OAuth2(config) = auth else {
            return Self::default();
        };
        let config = config.resolve(variables);
        let flow = flow.filter(|flow| flow.key == config.key());
        Self {
            token: tokens.get(&config),
            pending: flow.is_some(),
            link: flow.and_then(Flow::link),
            config: Some(config),
            action: None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let Some(config) = &self.config else {
            return;
        };
        let code = config.grant == Grant::AuthorizationCode;
        ui.add_space(8.0);
        ui.separator();
        ui.strong("Access token");
        if self.pending {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if code {
                    "Waiting for the authorization in your browser…"
                } else {
                    "Requesting a token…"
                });
                if ui.button("Cancel").clicked() {
                    self.action = Some(TokenAction::Cancel);
                }
            });
            if let Some(link) = &self.link {
                ui.horizontal(|ui| {
                    ui.weak("Browser did not open?");
                    if ui.button("📋 Copy the link").clicked() {
                        ui.ctx().copy_text(link.clone());
                    }
                });
            }
            return;
        }
        match &self.token {
            Some(token) => {
                let (status, expired) = match token.expires_at {
                    None => ("valid, no expiry given".to_owned(), false),
                    Some(at) if token.is_fresh() => (
                        format!("expires {}", format::until(at - format::unix_now())),
                        false,
                    ),
                    Some(_) if token.refresh_token.is_some() => {
                        ("expired, refreshed on the next send".to_owned(), true)
                    }
                    Some(_) => ("expired".to_owned(), true),
                };
                let color = if expired {
                    ui.visuals().warn_fg_color
                } else {
                    egui::Color32::from_rgb(80, 170, 90)
                };
                ui.colored_label(color, status);
                ui.horizontal(|ui| {
                    ui.monospace(format::truncate(&token.access_token, 48));
                    if ui
                        .small_button("📋")
                        .on_hover_text("Copy the access token")
                        .clicked()
                    {
                        ui.ctx().copy_text(token.access_token.clone());
                    }
                });
            }
            None if code => {
                ui.weak("Not authorized yet: requests fail until you authorize.");
            }
            None => {
                ui.weak("No token yet: one is requested on the first send.");
            }
        }
        ui.horizontal(|ui| {
            let label = match (code, self.token.is_some()) {
                (true, _) => "🔑 Authorize in browser",
                (false, false) => "🔑 Get token",
                (false, true) => "🔑 Get new token",
            };
            if ui.button(label).clicked() {
                self.action = Some(TokenAction::Get);
            }
            if self.token.is_some() && ui.button("Clear").clicked() {
                self.action = Some(TokenAction::Clear);
            }
        });
        ui.weak("Tokens are kept in .cornu/tokens.json, outside git.");
    }
}

/// A token being requested in the background.
pub struct Flow {
    key: String,
    result: Receiver<Result<Token, SendError>>,
    abort: AbortHandle,
    link: Arc<Mutex<Option<String>>>,
    opened: bool,
}

pub enum FlowEvent {
    Received,
    Failed(String),
}

impl Flow {
    pub fn start(ctx: &egui::Context, worker: &Worker, config: OAuth2) -> Self {
        let link = Arc::new(Mutex::new(None));
        let shared = link.clone();
        let repaint = ctx.clone();
        let key = config.key();
        let (result, abort) = worker.spawn(ctx, move |client| async move {
            client
                .new_token(&config, move |url| {
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = Some(url.to_string());
                    repaint.request_repaint();
                })
                .await
        });
        Self {
            key,
            result,
            abort,
            link,
            opened: false,
        }
    }

    fn link(&self) -> Option<String> {
        self.link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn cancel(self) {
        self.abort.abort();
    }

    /// Opens the authorization page once known; `None` while the flow goes on.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<FlowEvent> {
        if !self.opened
            && let Some(link) = self.link()
        {
            self.opened = true;
            ctx.open_url(egui::OpenUrl::new_tab(link));
        }
        match self.result.try_recv() {
            Ok(Ok(_)) => Some(FlowEvent::Received),
            Ok(Err(error)) => Some(FlowEvent::Failed(error.to_string())),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(FlowEvent::Failed("cancelled".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(panel: &mut TokenPanel) -> String {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| panel.show(ui),
        );
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    #[test]
    fn panel_tells_what_happens_next() {
        let config = OAuth2 {
            token_url: "{{base}}/token".into(),
            client_id: "app".into(),
            ..Default::default()
        };
        let auth = Auth::OAuth2(config.clone());
        let variables =
            Variables::new().with_layer([("base".to_owned(), "https://a.example".to_owned())]);
        let tokens = TokenStore::default();

        let mut panel = TokenPanel::new(&Auth::None, &variables, &tokens, None);
        assert!(panel.config.is_none());
        assert_eq!(render(&mut panel), "[]");

        let mut panel = TokenPanel::new(&auth, &variables, &tokens, None);
        assert_eq!(
            panel.config.as_ref().unwrap().token_url,
            "https://a.example/token"
        );
        let shapes = render(&mut panel);
        assert!(shapes.contains("first send"), "{shapes}");
        assert!(shapes.contains("Get token"));

        tokens.set(
            &config.resolve(&variables),
            Token {
                access_token: "abc123".into(),
                refresh_token: Some("r".into()),
                expires_at: Some(1),
            },
        );
        let mut panel = TokenPanel::new(&auth, &variables, &tokens, None);
        let shapes = render(&mut panel);
        assert!(shapes.contains("refreshed on the next send"), "{shapes}");
        assert!(shapes.contains("abc123"));
        assert!(shapes.contains("Get new token"));

        let mut code = config;
        code.grant = Grant::AuthorizationCode;
        let mut panel = TokenPanel::new(&Auth::OAuth2(code), &variables, &tokens, None);
        let shapes = render(&mut panel);
        assert!(shapes.contains("Not authorized yet"), "{shapes}");
        assert!(shapes.contains("Authorize in browser"));
    }
}
