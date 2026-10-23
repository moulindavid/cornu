use std::time::Instant;

use cornu_core::{Method, Request};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use tokio::task::AbortHandle;

use crate::editor::{self, EditorState};
use crate::environments::Environments;
use crate::format;
use crate::response::ResponseView;
use crate::widgets::{variable_field, vertical_split};
use crate::worker::{DocId, Worker};

const SEND_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);

enum SendState {
    Idle,
    Sending {
        abort: AbortHandle,
        started: Instant,
    },
    Done(Box<ResponseView>),
    Failed(String),
}

struct Document {
    id: DocId,
    request: Request,
    state: SendState,
    editor: EditorState,
    unresolved: Vec<String>,
}

impl Document {
    fn title(&self) -> &str {
        if self.request.name.is_empty() {
            "Untitled request"
        } else {
            &self.request.name
        }
    }
}

pub struct CornuApp {
    worker: Worker,
    documents: Vec<Document>,
    open_tabs: Vec<DocId>,
    active: Option<DocId>,
    next_id: DocId,
    response_split: f32,
    environments: Environments,
}

impl CornuApp {
    pub fn new(worker: Worker) -> Self {
        let mut app = Self {
            worker,
            documents: Vec::new(),
            open_tabs: Vec::new(),
            active: None,
            next_id: 0,
            response_split: 0.55,
            environments: Environments::default(),
        };
        app.new_request();
        app
    }

    fn new_request(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.documents.push(Document {
            id,
            request: Request::new(Method::Get, ""),
            state: SendState::Idle,
            editor: EditorState::default(),
            unresolved: Vec::new(),
        });
        self.open(id);
    }

    fn open(&mut self, id: DocId) {
        if !self.open_tabs.contains(&id) {
            self.open_tabs.push(id);
        }
        self.active = Some(id);
    }

    fn close(&mut self, id: DocId) {
        let Some(index) = self.open_tabs.iter().position(|tab| *tab == id) else {
            return;
        };
        self.open_tabs.remove(index);
        if self.active == Some(id) {
            self.active = self
                .open_tabs
                .get(index.min(self.open_tabs.len().saturating_sub(1)))
                .copied();
        }
    }

    fn document_mut(&mut self, id: DocId) -> Option<&mut Document> {
        self.documents.iter_mut().find(|doc| doc.id == id)
    }

    fn collect_completed(&mut self) {
        let completed: Vec<_> = self.worker.completed().collect();
        for done in completed {
            if let Some(doc) = self.document_mut(done.doc) {
                doc.state = match done.result {
                    Ok(response) => SendState::Done(Box::new(ResponseView::new(response))),
                    Err(error) => SendState::Failed(error.to_string()),
                };
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Requests");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ New").clicked() {
                    self.new_request();
                }
            });
        });
        ui.separator();
        let mut clicked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for doc in &self.documents {
                let selected = self.active == Some(doc.id);
                let label = format!("{}  {}", doc.request.method, doc.title());
                if ui.selectable_label(selected, label).clicked() {
                    clicked = Some(doc.id);
                }
            }
        });
        if let Some(id) = clicked {
            self.open(id);
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut activate = None;
        let mut close = None;
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for &id in &self.open_tabs {
                    let Some(doc) = self.documents.iter().find(|doc| doc.id == id) else {
                        continue;
                    };
                    let tab = ui.selectable_label(self.active == Some(id), doc.title());
                    if tab.clicked() {
                        activate = Some(id);
                    }
                    if tab.middle_clicked() || ui.small_button("×").clicked() {
                        close = Some(id);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(id) = activate {
            self.active = Some(id);
        }
        if let Some(id) = close {
            self.close(id);
        }
    }

    fn request_view(&mut self, ui: &mut egui::Ui, id: DocId) {
        let ctx = ui.ctx().clone();
        let send_pressed = ui.input_mut(|input| input.consume_shortcut(&SEND_SHORTCUT));
        let variables = self.environments.variables();
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return;
        };

        let mut send = send_pressed;
        let mut cancel = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("method", id))
                .selected_text(doc.request.method.as_str())
                .width(90.0)
                .show_ui(ui, |ui| {
                    for method in Method::STANDARD {
                        let label = method.to_string();
                        ui.selectable_value(&mut doc.request.method, method, label);
                    }
                });
            let sending = matches!(doc.state, SendState::Sending { .. });
            let button = if sending { "Cancel" } else { "Send" };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(button).clicked() {
                    if sending {
                        cancel = true;
                    } else {
                        send = true;
                    }
                }
                let url = variable_field(
                    ui,
                    &mut doc.request.url,
                    &variables,
                    "https://api.example.com/users",
                );
                if url.lost_focus() {
                    editor::extract_query(&mut doc.request.url, &mut doc.request.query);
                }
            });
        });

        if cancel && let SendState::Sending { abort, .. } = &doc.state {
            abort.abort();
            doc.state = SendState::Failed("Cancelled".into());
        } else if send && !matches!(doc.state, SendState::Sending { .. }) {
            let resolved = doc.request.resolve(&variables);
            doc.unresolved = resolved.unresolved;
            let abort = self.worker.send(&ctx, id, resolved.request);
            doc.state = SendState::Sending {
                abort,
                started: Instant::now(),
            };
        }

        ui.separator();
        let editor_id = egui::Id::unique(("editor", id));
        vertical_split(
            ui,
            egui::Id::unique("response-split"),
            &mut self.response_split,
            |ui| editor::request_editor(ui, editor_id, &mut doc.request, &mut doc.editor),
            |ui| {
                ui.add_space(6.0);
                if !doc.unresolved.is_empty() && !matches!(doc.state, SendState::Idle) {
                    let names = doc.unresolved.join(", ");
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("⚠ Sent with unresolved variables: {names}"),
                    );
                }
                response_view(ui, &mut doc.state);
            },
        );
    }
}

fn response_view(ui: &mut egui::Ui, state: &mut SendState) {
    match state {
        SendState::Idle => {
            ui.weak("Send a request to see the response (Ctrl+Enter).");
        }
        SendState::Sending { started, .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Sending… {}", format::duration(started.elapsed())));
            });
            ui.ctx().request_repaint();
        }
        SendState::Failed(message) => {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
        SendState::Done(view) => view.show(ui),
    }
}

impl eframe::App for CornuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.collect_completed();

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(240.0)
            .min_size(160.0)
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                let selector_width = 240.0;
                let tabs_width = (ui.available_width() - selector_width).max(120.0);
                let height = ui.spacing().interact_size.y;
                ui.allocate_ui(egui::vec2(tabs_width, height), |ui| self.tab_bar(ui));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.environments.selector(ui);
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| match self.active {
            Some(id) => self.request_view(ui, id),
            None => {
                ui.centered_and_justified(|ui| ui.weak("No request open"));
            }
        });

        self.environments.manager(ui.ctx());
    }
}
