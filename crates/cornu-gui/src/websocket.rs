use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use cornu_core::{Body, Response, WsCommand, WsEvent, WsMessage};
use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use crate::format;

pub enum SocketUpdate {
    Connected(Response),
    Event(WsEvent),
    Failed(String),
}

/// What the worker hands back for a new connection.
pub struct SocketHandle {
    pub commands: UnboundedSender<WsCommand>,
    pub updates: Receiver<SocketUpdate>,
    pub abort: AbortHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Connecting,
    Open(Box<Response>),
    Closed(String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Sent,
    Received,
}

struct Entry {
    direction: Direction,
    message: WsMessage,
    /// Since the connection started.
    at: Duration,
}

pub struct SocketView {
    handle: SocketHandle,
    status: Status,
    started: Instant,
    log: Vec<Entry>,
    filter: String,
}

pub enum SocketAction {
    Send,
    Disconnect,
}

impl Drop for SocketView {
    fn drop(&mut self) {
        self.handle.abort.abort();
    }
}

impl SocketView {
    pub fn new(handle: SocketHandle) -> Self {
        Self {
            handle,
            status: Status::Connecting,
            started: Instant::now(),
            log: Vec::new(),
            filter: String::new(),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self.status, Status::Connecting | Status::Open(_))
    }

    pub fn is_open(&self) -> bool {
        matches!(self.status, Status::Open(_))
    }

    pub fn send(&self, message: WsMessage) {
        let _ = self.handle.commands.send(WsCommand::Send(message));
    }

    /// Closes gracefully once open; a connection still being made is dropped.
    pub fn disconnect(&mut self) {
        if self.is_open() {
            let _ = self.handle.commands.send(WsCommand::Close);
        } else if self.is_live() {
            self.handle.abort.abort();
            self.status = Status::Closed("Cancelled".into());
        }
    }

    pub fn poll(&mut self) {
        loop {
            let update = match self.handle.updates.try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if self.is_live() {
                        self.status = Status::Closed("Disconnected".into());
                    }
                    return;
                }
            };
            let at = self.started.elapsed();
            match update {
                SocketUpdate::Connected(handshake) => {
                    self.status = Status::Open(Box::new(handshake))
                }
                SocketUpdate::Failed(error) => self.status = Status::Failed(error),
                SocketUpdate::Event(WsEvent::Sent(message)) => self.log.push(Entry {
                    direction: Direction::Sent,
                    message,
                    at,
                }),
                SocketUpdate::Event(WsEvent::Received(message)) => self.log.push(Entry {
                    direction: Direction::Received,
                    message,
                    at,
                }),
                SocketUpdate::Event(WsEvent::Closed { code, reason }) => {
                    let mut text = match code {
                        Some(code) => format!("Closed with code {code}"),
                        None => "Closed".to_owned(),
                    };
                    if !reason.is_empty() {
                        text.push_str(&format!(": {reason}"));
                    }
                    self.status = Status::Closed(text);
                }
                SocketUpdate::Event(WsEvent::Failed(error)) => self.status = Status::Failed(error),
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, message: &mut String) -> Option<SocketAction> {
        let mut action = None;
        ui.horizontal(|ui| match &self.status {
            Status::Connecting => {
                ui.spinner();
                ui.label(format!(
                    "Connecting… {}",
                    format::duration(self.started.elapsed())
                ));
                ui.ctx().request_repaint();
            }
            Status::Open(handshake) => {
                ui.colored_label(egui::Color32::from_rgb(80, 170, 90), "● Connected");
                ui.weak(format!(
                    "{} Switching Protocols · {}",
                    handshake.status,
                    format::duration(handshake.elapsed)
                ))
                .on_hover_ui(|ui| {
                    for (name, value) in &handshake.headers {
                        ui.monospace(format!("{name}: {value}"));
                    }
                });
            }
            Status::Closed(text) => {
                ui.weak(format!("○ {text}"));
            }
            Status::Failed(error) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("✖ {error}"));
            }
        });

        ui.add_space(4.0);
        let composer_height = 110.0;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter messages")
                    .desired_width(200.0),
            );
            let sent = self
                .log
                .iter()
                .filter(|entry| entry.direction == Direction::Sent)
                .count();
            ui.weak(format!("↑ {sent}  ↓ {}", self.log.len() - sent));
            if !self.log.is_empty() && ui.small_button("Clear").clicked() {
                self.log.clear();
            }
        });
        let filter = self.filter.to_lowercase();
        let log_height = (ui.available_height() - composer_height).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("socket-log")
            .max_height(log_height)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.log.is_empty() {
                    ui.weak("No messages yet.");
                }
                for entry in self.log.iter().filter(|entry| {
                    filter.is_empty() || text(&entry.message).to_lowercase().contains(&filter)
                }) {
                    entry_row(ui, entry);
                }
            });

        ui.separator();
        ui.add(
            egui::TextEdit::multiline(message)
                .hint_text("Message to send, {{variables}} allowed")
                .font(egui::TextStyle::Monospace)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let open = self.is_open();
            if ui
                .add_enabled(open, egui::Button::new("Send message"))
                .on_hover_text("Ctrl+Enter")
                .clicked()
            {
                action = Some(SocketAction::Send);
            }
            if self.is_live() && ui.button("Disconnect").clicked() {
                action = Some(SocketAction::Disconnect);
            }
        });
        action
    }
}

fn text(message: &WsMessage) -> String {
    match message {
        WsMessage::Text(text) => text.clone(),
        WsMessage::Binary(bytes) => {
            let hex: Vec<String> = bytes
                .iter()
                .take(64)
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let more = if bytes.len() > 64 { " …" } else { "" };
            format!(
                "<binary, {}> {}{more}",
                format::size(bytes.len()),
                hex.join(" ")
            )
        }
    }
}

fn entry_row(ui: &mut egui::Ui, entry: &Entry) {
    let (arrow, color) = match entry.direction {
        Direction::Sent => ("↑", egui::Color32::from_rgb(0xf0, 0xa0, 0x40)),
        Direction::Received => ("↓", egui::Color32::from_rgb(0x50, 0xa0, 0xf0)),
    };
    let body = text(&entry.message);
    ui.horizontal_top(|ui| {
        ui.colored_label(color, arrow);
        ui.weak(format!("{:>8.3}s", entry.at.as_secs_f64()));
        if ui.small_button("📋").on_hover_text("Copy").clicked() {
            ui.ctx().copy_text(body.clone());
        }
        let shown = format::pretty_json(&body).unwrap_or(body);
        ui.add(egui::Label::new(egui::RichText::new(shown).monospace()).wrap());
    });
}

/// The message composer keeps its text in the request body, so it is saved with the request.
pub fn message(body: &Body) -> &str {
    match body {
        Body::Json { text } | Body::Raw { text, .. } => text,
        _ => "",
    }
}

/// An empty message leaves a non-text body alone, so opening a request does not modify it.
pub fn set_message(body: &mut Body, message: String) {
    match body {
        Body::Json { text } | Body::Raw { text, .. } => *text = message,
        _ if message.is_empty() => {}
        _ => *body = Body::Json { text: message },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    fn view() -> (SocketView, std::sync::mpsc::Sender<SocketUpdate>) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let abort = runtime.spawn(async {}).abort_handle();
        let (commands, _) = tokio::sync::mpsc::unbounded_channel();
        let (tx, updates) = channel();
        let view = SocketView::new(SocketHandle {
            commands,
            updates,
            abort,
        });
        (view, tx)
    }

    fn render(view: &mut SocketView) -> String {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut message = "{\"type\": \"ping\"}".to_owned();
        let mut output = ctx.run_ui(input, |ui| {
            view.show(ui, &mut message);
        });
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    #[test]
    fn log_and_status_follow_the_updates() {
        let (mut view, tx) = view();
        assert!(render(&mut view).contains("Connecting"));

        let handshake = Response {
            status: 101,
            headers: Vec::new(),
            body: Vec::new(),
            elapsed: Duration::from_millis(12),
        };
        tx.send(SocketUpdate::Connected(handshake)).unwrap();
        tx.send(SocketUpdate::Event(WsEvent::Sent(WsMessage::Text(
            "hello".into(),
        ))))
        .unwrap();
        tx.send(SocketUpdate::Event(WsEvent::Received(WsMessage::Text(
            "{\"id\":7}".into(),
        ))))
        .unwrap();
        tx.send(SocketUpdate::Event(WsEvent::Received(WsMessage::Binary(
            vec![1, 255],
        ))))
        .unwrap();
        view.poll();
        assert!(view.is_open());
        let shapes = render(&mut view);
        for expected in [
            "Connected",
            "101",
            "hello",
            "\\\"id\\\": 7",
            "01 ff",
            "↑ 1",
            "↓ 2",
            "Send message",
        ] {
            assert!(shapes.contains(expected), "{expected} in {shapes}");
        }

        view.filter = "hello".into();
        let shapes = render(&mut view);
        assert!(shapes.contains("hello") && !shapes.contains("01 ff"));

        tx.send(SocketUpdate::Event(WsEvent::Closed {
            code: Some(4000),
            reason: "bye".into(),
        }))
        .unwrap();
        view.poll();
        assert!(!view.is_live());
        assert!(render(&mut view).contains("Closed with code 4000: bye"));
    }

    #[test]
    fn composer_text_lives_in_the_body() {
        let mut body = Body::None;
        set_message(&mut body, String::new());
        assert_eq!(body, Body::None);
        set_message(&mut body, "hi".into());
        assert_eq!(body, Body::Json { text: "hi".into() });
        let mut raw = Body::Raw {
            text: "x".into(),
            content_type: "text/plain".into(),
        };
        assert_eq!(message(&raw), "x");
        set_message(&mut raw, String::new());
        assert_eq!(message(&raw), "");
    }

    #[test]
    fn worker_connects_sends_and_receives() {
        use futures_util::{SinkExt, StreamExt};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let url = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/", listener.local_addr().unwrap());
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if message.is_text() {
                        socket.send(message).await.unwrap();
                    }
                }
            });
            url
        });
        let worker = crate::worker::Worker::new().unwrap();
        let ctx = egui::Context::default();
        let request = cornu_core::Request::new(cornu_core::Method::Get, url);
        let mut view = SocketView::new(worker.websocket(&ctx, request));
        let wait = |view: &mut SocketView, done: &dyn Fn(&SocketView) -> bool| {
            for _ in 0..250 {
                view.poll();
                if done(view) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("timed out, status {:?}", view.status);
        };
        wait(&mut view, &|view| view.is_open());
        view.send(WsMessage::Text("ping".into()));
        wait(&mut view, &|view| view.log.len() == 2);
        assert_eq!(view.log[1].direction, Direction::Received);
        assert_eq!(view.log[1].message, WsMessage::Text("ping".into()));

        view.disconnect();
        wait(&mut view, &|view| !view.is_live());
        assert_eq!(view.status, Status::Closed("Closed with code 1000".into()));
    }
}
