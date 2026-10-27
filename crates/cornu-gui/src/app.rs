use std::path::{Path, PathBuf};
use std::time::Instant;

use cornu_core::{Method, Request};
use cornu_storage::{HistoryEntry, Outcome, moved_path};
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use tokio::task::AbortHandle;

use crate::editor::{self, EditorState};
use crate::environments::Environments;
use crate::folder::{FolderTab, folder_view};
use crate::format;
use crate::history::{HistoryAction, HistoryPanel};
use crate::project::Project;
use crate::response::ResponseView;
use crate::tree::{Tree, TreeAction, method_color};
use crate::widgets::{variable_field, vertical_split};
use crate::worker::{DocId, Worker};

const SEND_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const SAVE_SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const WORKSPACE_KEY: &str = "workspace";
const NEW_REQUEST: &str = "New request";

enum SendState {
    Idle,
    Sending {
        abort: AbortHandle,
        started: Instant,
        /// The request as edited when it was sent, recorded in the history.
        sent: Box<Request>,
        timestamp: i64,
    },
    Done(Box<ResponseView>),
    Failed(String),
}

struct RequestDoc {
    request: Request,
    /// What is on disk; `None` for a draft that was never saved.
    saved: Option<Request>,
    state: SendState,
    editor: EditorState,
    unresolved: Vec<String>,
}

impl RequestDoc {
    fn new(request: Request, saved: bool) -> Self {
        Self {
            saved: saved.then(|| request.clone()),
            request,
            state: SendState::Idle,
            editor: EditorState::default(),
            unresolved: Vec::new(),
        }
    }

    fn is_modified(&self) -> bool {
        self.saved
            .as_ref()
            .is_some_and(|saved| *saved != self.request)
    }

    fn title(&self) -> &str {
        if self.request.name.is_empty() {
            "Untitled request"
        } else {
            &self.request.name
        }
    }
}

enum Content {
    Request(Box<RequestDoc>),
    Folder(FolderTab),
}

struct Document {
    id: DocId,
    /// Relative to the workspace root; `None` for drafts.
    path: Option<PathBuf>,
    content: Content,
}

impl Document {
    fn request(&self) -> Option<&RequestDoc> {
        match &self.content {
            Content::Request(doc) => Some(doc),
            Content::Folder(_) => None,
        }
    }

    fn is_modified(&self) -> bool {
        self.request().is_some_and(RequestDoc::is_modified)
    }

    fn is_draft(&self) -> bool {
        self.path.is_none()
    }
}

pub struct CornuApp {
    worker: Worker,
    project: Project,
    documents: Vec<Document>,
    open_tabs: Vec<DocId>,
    active: Option<DocId>,
    next_id: DocId,
    response_split: f32,
    environments: Environments,
    tree: Tree,
    sidebar_tab: SidebarTab,
    history: HistoryPanel,
    notice: Option<Notice>,
    closing: Option<DocId>,
    deleting: Option<PathBuf>,
}

impl CornuApp {
    pub fn new(worker: Worker, workspace: Option<PathBuf>) -> Self {
        let mut app = Self {
            worker,
            project: Project::scratch(),
            documents: Vec::new(),
            open_tabs: Vec::new(),
            active: None,
            next_id: 0,
            response_split: 0.55,
            environments: Environments::default(),
            tree: Tree::default(),
            sidebar_tab: SidebarTab::default(),
            history: HistoryPanel::default(),
            notice: None,
            closing: None,
            deleting: None,
        };
        if let Some(root) = workspace {
            app.switch_workspace(Project::open(&root));
        }
        if app.documents.is_empty() && !app.project.is_on_disk() {
            app.new_draft();
        }
        app
    }

    fn add_document(&mut self, path: Option<PathBuf>, content: Content) -> DocId {
        let id = self.next_id;
        self.next_id += 1;
        self.documents.push(Document { id, path, content });
        self.open(id);
        id
    }

    fn new_draft(&mut self) {
        self.open_draft(Request::new(Method::Get, ""));
    }

    fn open_draft(&mut self, request: Request) {
        self.add_document(
            None,
            Content::Request(Box::new(RequestDoc::new(request, false))),
        );
    }

    fn open(&mut self, id: DocId) {
        if !self.open_tabs.contains(&id) {
            self.open_tabs.push(id);
        }
        self.active = Some(id);
    }

    fn find_by_path(&self, path: &Path, folder: bool) -> Option<DocId> {
        self.documents
            .iter()
            .find(|doc| {
                doc.path.as_deref() == Some(path)
                    && matches!(doc.content, Content::Folder(_)) == folder
            })
            .map(|doc| doc.id)
    }

    fn open_request(&mut self, path: &Path) {
        if let Some(id) = self.find_by_path(path, false) {
            return self.open(id);
        }
        if let Some(entry) = self.project.workspace.find_request(path) {
            let doc = RequestDoc::new(entry.request.clone(), true);
            self.add_document(Some(path.to_owned()), Content::Request(Box::new(doc)));
        }
    }

    fn open_folder(&mut self, path: &Path) {
        match self.find_by_path(path, true) {
            Some(id) => self.open(id),
            None => {
                self.add_document(Some(path.to_owned()), Content::Folder(FolderTab::default()));
            }
        }
    }

    fn request_close(&mut self, id: DocId) {
        match self.documents.iter().find(|doc| doc.id == id) {
            Some(doc) if doc.is_modified() => self.closing = Some(id),
            _ => self.close(id),
        }
    }

    /// Drafts stay listed in the sidebar; saved documents can be reopened from the tree.
    fn close(&mut self, id: DocId) {
        if let Some(index) = self.open_tabs.iter().position(|tab| *tab == id) {
            self.open_tabs.remove(index);
            if self.active == Some(id) {
                self.active = self
                    .open_tabs
                    .get(index.min(self.open_tabs.len().saturating_sub(1)))
                    .copied();
            }
        }
        self.documents.retain(|doc| doc.id != id || doc.is_draft());
    }

    fn discard(&mut self, id: DocId) {
        self.close(id);
        self.documents.retain(|doc| doc.id != id);
    }

    fn save(&mut self, id: DocId) {
        if let Err(error) = self.try_save(id) {
            self.notice = Some(Notice::error(error));
        }
    }

    fn try_save(&mut self, id: DocId) -> Result<(), String> {
        let on_disk = self.project.is_on_disk();
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return Ok(());
        };
        let Content::Request(request_doc) = &mut doc.content else {
            return Ok(());
        };
        let workspace = &mut self.project.workspace;
        match &doc.path {
            Some(path) => {
                workspace
                    .update_request(path, request_doc.request.clone())
                    .map_err(|error| error.to_string())?;
            }
            None if on_disk => {
                let unnamed = request_doc.request.name.trim().is_empty();
                if unnamed {
                    request_doc.request.name = NEW_REQUEST.into();
                }
                let path = workspace
                    .create_request(Path::new(""), request_doc.request.clone())
                    .map_err(|error| error.to_string())?;
                if unnamed {
                    self.tree.start_rename(path.clone(), NEW_REQUEST);
                }
                doc.path = Some(path);
            }
            None => return Err("Open or create a workspace to save requests.".into()),
        }
        request_doc.saved = Some(request_doc.request.clone());
        Ok(())
    }

    fn pick_workspace(&mut self, create: bool) {
        let Some(root) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let project = if create {
            Project::create(&root)
        } else {
            Project::open(&root)
        };
        self.switch_workspace(project);
    }

    fn import_postman(&mut self, ctx: &egui::Context) {
        if !self.project.is_on_disk() {
            self.notice = Some(Notice::error(
                "Open or create a workspace first: the collection is imported into it.",
            ));
            return;
        }
        let Some(file) = rfd::FileDialog::new()
            .set_title("Import Postman collection")
            .add_filter("Postman collection", &["json"])
            .pick_file()
        else {
            return;
        };
        self.notice = Some(
            match import_postman_file(&file, &mut self.project.workspace) {
                Ok((path, collection)) => {
                    Tree::expand(ctx, &path);
                    Notice {
                        message: format!(
                            "Imported \"{}\": {} requests{}",
                            collection.root.name,
                            collection.root.request_count(),
                            match collection.warnings.len() {
                                0 => String::new(),
                                count => format!(", {count} warning(s)"),
                            }
                        ),
                        details: collection.warnings,
                        error: false,
                    }
                }
                Err(error) => Notice::error(format!("Import failed: {error}")),
            },
        );
    }

    fn import_postman_environments(&mut self) {
        let Some(files) = rfd::FileDialog::new()
            .set_title("Import Postman environments")
            .add_filter("Postman environment", &["json"])
            .pick_files()
        else {
            return;
        };
        let mut imported = Vec::new();
        let mut failures = Vec::new();
        for file in files {
            match import_postman_environment(&file, &mut self.project) {
                Ok(name) => imported.push(name),
                Err(error) => failures.push(format!("{}: {error}", file.display())),
            }
        }
        let message = match (imported.as_slice(), failures.len()) {
            ([], _) => "Environment import failed".to_owned(),
            ([name], 0) => format!("Imported environment \"{name}\""),
            (names, 0) => format!("Imported {} environments", names.len()),
            (names, failed) => format!("Imported {} environment(s), {failed} failed", names.len()),
        };
        self.notice = Some(Notice {
            message,
            error: imported.is_empty(),
            details: failures,
        });
    }

    fn close_workspace(&mut self) {
        self.switch_workspace(Ok(Project::scratch()));
    }

    fn switch_workspace(&mut self, project: Result<Project, cornu_storage::StorageError>) {
        if self.documents.iter().any(Document::is_modified) {
            self.notice = Some(Notice::error(
                "Save or close modified requests before switching workspace.",
            ));
            return;
        }
        let project = match project {
            Ok(project) => project,
            Err(error) => {
                self.notice = Some(Notice::error(error.to_string()));
                return;
            }
        };
        let saved: Vec<_> = self
            .documents
            .iter()
            .filter(|doc| !doc.is_draft())
            .map(|doc| doc.id)
            .collect();
        for id in saved {
            self.discard(id);
        }
        self.notice = match project.workspace.problems.as_slice() {
            [] => None,
            [problem] => Some(Notice::error(format!("Could not read {problem}"))),
            problems => Some(Notice {
                message: format!("Could not read {} files", problems.len()),
                details: problems.to_vec(),
                error: true,
            }),
        };
        self.project = project;
        self.environments.reset();
        self.tree = Tree::default();
    }

    fn apply(&mut self, ctx: &egui::Context, action: TreeAction) {
        if let Err(error) = self.try_apply(ctx, action) {
            self.notice = Some(Notice::error(error.to_string()));
        }
    }

    fn try_apply(
        &mut self,
        ctx: &egui::Context,
        action: TreeAction,
    ) -> Result<(), cornu_storage::StorageError> {
        let workspace = &mut self.project.workspace;
        match action {
            TreeAction::OpenRequest(path) => self.open_request(&path),
            TreeAction::OpenFolder(path) => self.open_folder(&path),
            TreeAction::NewRequest(parent) => {
                let request = Request::new(Method::Get, "").with_name(NEW_REQUEST);
                let path = workspace.create_request(&parent, request)?;
                Tree::expand(ctx, &parent);
                self.tree.start_rename(path.clone(), NEW_REQUEST);
                self.open_request(&path);
            }
            TreeAction::NewFolder(parent) => {
                let path = workspace.create_folder(&parent, "New folder")?;
                Tree::expand(ctx, &parent);
                self.tree.start_rename(path, "New folder");
            }
            TreeAction::Rename(path, name) => {
                let new_path = workspace.rename(&path, &name)?;
                for doc in &mut self.documents {
                    let Some(moved) = doc
                        .path
                        .as_deref()
                        .and_then(|p| moved_path(p, &path, &new_path))
                    else {
                        continue;
                    };
                    if let Content::Request(request) = &mut doc.content
                        && moved == new_path
                    {
                        request.request.name.clone_from(&name);
                        if let Some(saved) = &mut request.saved {
                            saved.name.clone_from(&name);
                        }
                    }
                    doc.path = Some(moved);
                }
            }
            TreeAction::Duplicate(path) => {
                let copy = workspace.duplicate(&path)?;
                if workspace.find_request(&copy).is_some() {
                    self.open_request(&copy);
                }
            }
            TreeAction::Delete(path) => self.deleting = Some(path),
        }
        Ok(())
    }

    fn delete(&mut self, path: &Path) {
        if let Err(error) = self.project.workspace.delete(path) {
            self.notice = Some(Notice::error(error.to_string()));
            return;
        }
        let removed: Vec<_> = self
            .documents
            .iter()
            .filter(|doc| doc.path.as_deref().is_some_and(|p| p.starts_with(path)))
            .map(|doc| doc.id)
            .collect();
        for id in removed {
            self.discard(id);
        }
    }

    fn collect_completed(&mut self) {
        let completed: Vec<_> = self.worker.completed().collect();
        for done in completed {
            let Some(document) = self.documents.iter_mut().find(|doc| doc.id == done.doc) else {
                continue;
            };
            let Content::Request(doc) = &mut document.content else {
                continue;
            };
            // A late result for a cancelled request is dropped.
            let SendState::Sending {
                sent, timestamp, ..
            } = &doc.state
            else {
                continue;
            };
            let outcome = match &done.result {
                Ok(response) => Outcome::Response {
                    status: response.status,
                    elapsed_ms: response.elapsed.as_millis() as u64,
                    size: response.size() as u64,
                },
                Err(error) => Outcome::Error {
                    message: error.to_string(),
                },
            };
            let entry = HistoryEntry {
                timestamp: *timestamp,
                request: (**sent).clone(),
                source: document.path.clone(),
                outcome,
            };
            doc.state = match done.result {
                Ok(response) => SendState::Done(Box::new(ResponseView::new(response))),
                Err(error) => SendState::Failed(error.to_string()),
            };
            if let Err(error) = self.project.history.push(entry) {
                self.notice = Some(Notice::error(error.to_string()));
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let title = if self.project.is_on_disk() {
                self.project.name()
            } else {
                "No workspace"
            };
            ui.heading(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("☰", |ui| {
                    if ui.button("Open workspace…").clicked() {
                        self.pick_workspace(false);
                    }
                    if ui.button("New workspace…").clicked() {
                        self.pick_workspace(true);
                    }
                    if self.project.is_on_disk() && ui.button("Close workspace").clicked() {
                        self.close_workspace();
                    }
                    ui.separator();
                    if ui.button("Import Postman collection…").clicked() {
                        self.import_postman(ui.ctx());
                    }
                    if ui.button("Import Postman environment…").clicked() {
                        self.import_postman_environments();
                    }
                });
            });
        });
        if let Some(root) = self.project.root() {
            let path = egui::RichText::new(root.display().to_string()).weak();
            ui.add(egui::Label::new(path).truncate());
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Collection, "Collection");
            ui.selectable_value(&mut self.sidebar_tab, SidebarTab::History, "History");
        });
        ui.separator();
        if self.sidebar_tab == SidebarTab::History {
            let action = egui::ScrollArea::vertical()
                .show(ui, |ui| self.history.show(ui, &self.project.history))
                .inner;
            if let Some(action) = action {
                self.apply_history(action);
            }
            return;
        }

        let active = self
            .active
            .and_then(|id| self.documents.iter().find(|doc| doc.id == id))
            .and_then(|doc| doc.path.clone());
        let mut actions = Vec::new();
        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.project.is_on_disk() {
                ui.horizontal(|ui| {
                    if ui.button("+ Request").clicked() {
                        actions.push(TreeAction::NewRequest(PathBuf::new()));
                    }
                    if ui.button("+ Folder").clicked() {
                        actions.push(TreeAction::NewFolder(PathBuf::new()));
                    }
                    if ui
                        .button("⚙")
                        .on_hover_text("Workspace auth and variables")
                        .clicked()
                    {
                        actions.push(TreeAction::OpenFolder(PathBuf::new()));
                    }
                });
                ui.add_space(4.0);
                let tree = &self.project.workspace.tree;
                actions.extend(self.tree.show(ui, tree, active.as_deref()));
                ui.add_space(8.0);
            } else {
                ui.label("Requests are kept as plain files you can commit to git.");
                ui.horizontal(|ui| {
                    if ui.button("Open workspace…").clicked() {
                        self.pick_workspace(false);
                    }
                    if ui.button("New workspace…").clicked() {
                        self.pick_workspace(true);
                    }
                });
                ui.add_space(8.0);
            }
            self.drafts(ui);
        });
        for action in actions {
            self.apply(ui.ctx(), action);
        }
    }

    fn apply_history(&mut self, action: HistoryAction) {
        match action {
            HistoryAction::Open(index) => {
                if let Some(entry) = self.project.history.entries().get(index) {
                    self.open_draft(entry.request.clone());
                }
            }
            HistoryAction::OpenSource(path) => {
                if self.project.workspace.find_request(&path).is_some() {
                    self.open_request(&path);
                } else {
                    self.notice = Some(Notice::error(format!(
                        "{} no longer exists.",
                        path.display()
                    )));
                }
            }
            HistoryAction::Clear => {
                if let Err(error) = self.project.history.clear() {
                    self.notice = Some(Notice::error(error.to_string()));
                }
            }
        }
    }

    fn drafts(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Drafts");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+").on_hover_text("New draft").clicked() {
                    self.new_draft();
                }
            });
        });
        let mut clicked = None;
        let mut removed = None;
        for doc in self.documents.iter().filter(|doc| doc.is_draft()) {
            let Some(request) = doc.request() else {
                continue;
            };
            let selected = self.active == Some(doc.id);
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &format!("{} ", request.request.method),
                0.0,
                egui::TextFormat::simple(
                    egui::FontId::monospace(10.0),
                    method_color(&request.request.method, ui.visuals()),
                ),
            );
            job.append(
                request.title(),
                0.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Button.resolve(ui.style()),
                    ui.visuals().text_color(),
                ),
            );
            let label = ui.selectable_label(selected, job);
            if label.clicked() {
                clicked = Some(doc.id);
            }
            label.context_menu(|ui| {
                if ui.button("Delete draft").clicked() {
                    removed = Some(doc.id);
                }
            });
        }
        if let Some(id) = clicked {
            self.open(id);
        }
        if let Some(id) = removed {
            self.discard(id);
        }
    }

    fn tab_title(&self, doc: &Document) -> String {
        match &doc.content {
            Content::Request(request) if request.is_modified() => format!("● {}", request.title()),
            Content::Request(request) => request.title().to_owned(),
            Content::Folder(_) => {
                let path = doc.path.as_deref().unwrap_or(Path::new(""));
                let name = self
                    .project
                    .workspace
                    .folder(path)
                    .map_or("Folder", |folder| folder.settings.name.as_str());
                if path.as_os_str().is_empty() {
                    format!("⚙ {name}")
                } else {
                    format!("📁 {name}")
                }
            }
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
                    let tab = ui.selectable_label(self.active == Some(id), self.tab_title(doc));
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
            self.request_close(id);
        }
    }

    fn document_view(&mut self, ui: &mut egui::Ui, id: DocId) {
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return;
        };
        match &mut doc.content {
            Content::Folder(tab) => {
                let path = doc.path.clone().unwrap_or_default();
                if let Some(error) = folder_view(ui, &mut self.project, &path, tab) {
                    self.notice = Some(Notice::error(error));
                }
            }
            Content::Request(_) => self.request_view(ui, id),
        }
    }

    fn request_view(&mut self, ui: &mut egui::Ui, id: DocId) {
        let ctx = ui.ctx().clone();
        let send_pressed = ui.input_mut(|input| input.consume_shortcut(&SEND_SHORTCUT));
        let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == id) else {
            return;
        };
        let variables = self
            .project
            .variables(doc.path.as_deref(), self.environments.active);
        let path = doc.path.clone();
        let Content::Request(doc) = &mut doc.content else {
            return;
        };

        let mut send = send_pressed;
        let mut cancel = false;
        let mut save = false;
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
                let save_button = ui.add_enabled(
                    doc.saved.is_none() || doc.is_modified(),
                    egui::Button::new("Save"),
                );
                if save_button.on_hover_text("Ctrl+S").clicked() {
                    save = true;
                }
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
            let mut request = doc.request.clone();
            request.auth = self.project.effective_auth(path.as_deref(), &request.auth);
            let resolved = request.resolve(&variables);
            doc.unresolved = resolved.unresolved;
            let abort = self.worker.send(&ctx, id, resolved.request);
            doc.state = SendState::Sending {
                abort,
                started: Instant::now(),
                sent: Box::new(doc.request.clone()),
                timestamp: format::unix_now(),
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
        if save {
            self.save(id);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| match &self.notice {
            Some(notice) => {
                let color = if notice.error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                let label = ui.colored_label(color, &notice.message);
                if !notice.details.is_empty() {
                    label.on_hover_ui(|ui| {
                        for detail in &notice.details {
                            ui.label(format!("• {detail}"));
                        }
                    });
                    ui.weak("(hover for details)");
                }
                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                    self.notice = None;
                }
            }
            None => {
                let modified = self
                    .documents
                    .iter()
                    .filter(|doc| doc.is_modified())
                    .count();
                if modified > 0 {
                    ui.weak(format!("{modified} unsaved request(s)"));
                }
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.closing {
            let title = self
                .documents
                .iter()
                .find(|doc| doc.id == id)
                .map(|doc| self.tab_title(doc))
                .unwrap_or_default();
            let mut choice = None;
            let modal = egui::Modal::new(egui::Id::unique("close-modified")).show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.heading("Unsaved changes");
                ui.label(format!(
                    "Save the changes to {}?",
                    title.trim_start_matches("● ")
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(Close::Save);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(Close::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(Close::Cancel);
                    }
                });
            });
            if modal.should_close() {
                choice = Some(Close::Cancel);
            }
            match choice {
                Some(Close::Save) => {
                    self.save(id);
                    if !self
                        .documents
                        .iter()
                        .any(|doc| doc.id == id && doc.is_modified())
                    {
                        self.discard(id);
                    }
                    self.closing = None;
                }
                Some(Close::Discard) => {
                    self.discard(id);
                    self.closing = None;
                }
                Some(Close::Cancel) => self.closing = None,
                None => {}
            }
        }

        if let Some(path) = self.deleting.clone() {
            let workspace = &self.project.workspace;
            let (kind, name) = match workspace.folder(&path) {
                Some(folder) => ("folder", folder.settings.name.clone()),
                None => (
                    "request",
                    workspace
                        .find_request(&path)
                        .map(|entry| entry.request.name.clone())
                        .unwrap_or_default(),
                ),
            };
            let mut confirmed = None;
            let modal = egui::Modal::new(egui::Id::unique("confirm-delete")).show(ctx, |ui| {
                ui.set_max_width(360.0);
                ui.heading(format!("Delete {kind}?"));
                if kind == "folder" {
                    ui.label(format!(
                        "\"{name}\" and everything inside it will be deleted."
                    ));
                } else {
                    ui.label(format!("\"{name}\" will be deleted."));
                }
                ui.weak(path.display().to_string());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        confirmed = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        confirmed = Some(false);
                    }
                });
            });
            if modal.should_close() {
                confirmed = Some(false);
            }
            if let Some(confirmed) = confirmed {
                self.deleting = None;
                if confirmed {
                    self.delete(&path);
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    #[default]
    Collection,
    History,
}

struct Notice {
    message: String,
    details: Vec<String>,
    error: bool,
}

impl Notice {
    fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: Vec::new(),
            error: true,
        }
    }
}

enum Close {
    Save,
    Discard,
    Cancel,
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
        if ui.input_mut(|input| input.consume_shortcut(&SAVE_SHORTCUT))
            && let Some(id) = self.active
        {
            self.save(id);
        }

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(260.0)
            .min_size(180.0)
            .show(ui, |ui| self.sidebar(ui));

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                let selector_width = 240.0;
                let tabs_width = (ui.available_width() - selector_width).max(120.0);
                let height = ui.spacing().interact_size.y;
                ui.allocate_ui(egui::vec2(tabs_width, height), |ui| self.tab_bar(ui));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.environments.selector(ui, &self.project);
                });
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        egui::CentralPanel::default().show(ui, |ui| match self.active {
            Some(id) => self.document_view(ui, id),
            None => {
                ui.centered_and_justified(|ui| ui.weak("No request open"));
            }
        });

        if let Some(error) = self.environments.manager(ui.ctx(), &mut self.project) {
            self.notice = Some(Notice::error(error));
        }
        self.dialogs(ui.ctx());
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let root = self.project.root().map(|root| root.display().to_string());
        storage.set_string(WORKSPACE_KEY, root.unwrap_or_default());
    }
}

pub fn last_workspace(storage: Option<&dyn eframe::Storage>) -> Option<PathBuf> {
    storage
        .and_then(|storage| storage.get_string(WORKSPACE_KEY))
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}

fn import_postman_environment(file: &Path, project: &mut Project) -> Result<String, String> {
    let json = std::fs::read_to_string(file).map_err(|error| error.to_string())?;
    let environment =
        cornu_import::postman::parse_environment(&json).map_err(|error| error.to_string())?;
    let name = environment.name.clone();
    project
        .add_environment(environment)
        .map_err(|error| error.to_string())?;
    Ok(name)
}

fn import_postman_file(
    file: &Path,
    workspace: &mut cornu_storage::Workspace,
) -> Result<(PathBuf, cornu_import::Collection), String> {
    let json = std::fs::read_to_string(file).map_err(|error| error.to_string())?;
    let collection =
        cornu_import::postman::parse_collection(&json).map_err(|error| error.to_string())?;
    let path = collection
        .root
        .write_into(workspace, Path::new(""))
        .map_err(|error| error.to_string())?;
    Ok((path, collection))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postman_environment_imports_into_scratch_project() {
        let mut project = Project::scratch();
        let file = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../cornu-import/tests/fixtures/postman-environment.json"
        ));
        assert_eq!(
            import_postman_environment(file, &mut project).unwrap(),
            "Pet Store Dev"
        );
        let environment = project.environment(Some(0)).unwrap();
        assert!(environment.variables.iter().any(|kv| kv.secret));

        let error = import_postman_environment(Path::new("missing.json"), &mut project);
        assert!(error.is_err());
        assert_eq!(project.workspace.environments.len(), 1);
    }
}
