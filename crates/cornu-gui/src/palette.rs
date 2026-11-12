use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

use crate::worker::DocId;

const COMMAND: Modifiers = Modifiers::COMMAND;
const COMMAND_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};

pub const SEND: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::Enter);
pub const SAVE: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::S);
pub const PALETTE: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::K);
pub const PALETTE_ALT: KeyboardShortcut = KeyboardShortcut::new(COMMAND_SHIFT, Key::P);
pub const GO_TO: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::P);
pub const ENVIRONMENT: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::E);
pub const NEW_REQUEST: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::N);
pub const CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::W);
pub const NEXT_TAB: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::PageDown);
pub const PREVIOUS_TAB: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::PageUp);
pub const NEXT_TAB_ALT: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::Tab);
pub const PREVIOUS_TAB_ALT: KeyboardShortcut = KeyboardShortcut::new(COMMAND_SHIFT, Key::Tab);
pub const FOCUS_URL: KeyboardShortcut = KeyboardShortcut::new(COMMAND, Key::L);
pub const SHORTCUTS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    OpenRequest(PathBuf),
    OpenFolder(PathBuf),
    ShowTab(DocId),
    UseEnvironment(Option<usize>),
    NewRequest,
    Save,
    Send,
    CopyAsCurl,
    RunWithData,
    CloseTab,
    NextTab,
    PreviousTab,
    FocusUrl,
    ManageEnvironments,
    OpenWorkspace,
    NewWorkspace,
    CloseWorkspace,
    WorkspaceSettings,
    RunWorkspace,
    ImportPostman,
    ImportPostmanEnvironment,
    ImportOpenApiFile,
    ImportOpenApiUrl,
    ClientSettings,
    Cookies,
    ShowCollection,
    ShowHistory,
    Shortcuts,
}

/// The shortcut of a command, shown next to it.
pub fn shortcut(command: &Command) -> Option<KeyboardShortcut> {
    Some(match command {
        Command::NewRequest => NEW_REQUEST,
        Command::Save => SAVE,
        Command::Send => SEND,
        Command::CloseTab => CLOSE_TAB,
        Command::NextTab => NEXT_TAB,
        Command::PreviousTab => PREVIOUS_TAB,
        Command::FocusUrl => FOCUS_URL,
        Command::Shortcuts => SHORTCUTS,
        _ => return None,
    })
}

/// Every shortcut with what it does, for the help window.
pub const SHORTCUT_HELP: [(&str, &[KeyboardShortcut]); 12] = [
    ("Command palette", &[PALETTE, PALETTE_ALT]),
    ("Go to a request or tab", &[GO_TO]),
    ("Switch environment", &[ENVIRONMENT]),
    ("Send the request, or the WebSocket message", &[SEND]),
    ("Save", &[SAVE]),
    ("New request", &[NEW_REQUEST]),
    ("Close the tab", &[CLOSE_TAB]),
    ("Next tab", &[NEXT_TAB, NEXT_TAB_ALT]),
    ("Previous tab", &[PREVIOUS_TAB, PREVIOUS_TAB_ALT]),
    ("Focus the URL", &[FOCUS_URL]),
    ("Paste a curl command in the URL to fill the request", &[]),
    ("Keyboard shortcuts", &[SHORTCUTS]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// A path, a URL or a kind, matched too and shown dimmed.
    pub detail: String,
    pub command: Command,
}

impl Entry {
    pub fn new(label: impl Into<String>, detail: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            command,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Commands, then requests and tabs.
    #[default]
    Everything,
    Requests,
    Environments,
}

impl Mode {
    fn accepts(self, command: &Command) -> bool {
        match self {
            Mode::Everything => true,
            Mode::Requests => matches!(command, Command::OpenRequest(_) | Command::ShowTab(_)),
            Mode::Environments => matches!(command, Command::UseEnvironment(_)),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Mode::Everything => "Type a command, a request or an environment",
            Mode::Requests => "Go to a request or a tab",
            Mode::Environments => "Switch to an environment",
        }
    }
}

#[derive(Default)]
pub struct Palette {
    open: bool,
    mode: Mode,
    query: String,
    selected: usize,
    focus: bool,
}

const SHOWN: usize = 60;

impl Palette {
    pub fn open(&mut self, mode: Mode) {
        *self = Self {
            open: true,
            mode,
            focus: true,
            ..Self::default()
        };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn matches<'a>(&self, entries: &'a [Entry]) -> Vec<&'a Entry> {
        let mut scored: Vec<(i32, usize, &Entry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.mode.accepts(&entry.command))
            .filter_map(|(index, entry)| {
                let text = format!("{} {}", entry.label, entry.detail);
                Some((score(&self.query, &text)?, index, entry))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(SHOWN)
            .map(|(_, _, entry)| entry)
            .collect()
    }

    pub fn show(&mut self, ctx: &egui::Context, entries: &[Entry]) -> Option<Command> {
        if !self.open {
            return None;
        }
        let (down, up, enter, escape) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Enter),
                input.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if escape {
            self.open = false;
            return None;
        }
        let mut chosen = None;
        let width = (ctx.content_rect().width() - 32.0).clamp(240.0, 560.0);
        let area = egui::Area::new(egui::Id::unique("command-palette"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text(self.mode.hint())
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.focus) {
                        edit.request_focus();
                    }
                    if edit.changed() {
                        self.selected = 0;
                    }
                    let matches = self.matches(entries);
                    if down && self.selected + 1 < matches.len() {
                        self.selected += 1;
                    }
                    if up {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    self.selected = self.selected.min(matches.len().saturating_sub(1));
                    if enter {
                        chosen = matches
                            .get(self.selected)
                            .map(|entry| entry.command.clone());
                    }
                    ui.separator();
                    if matches.is_empty() {
                        ui.weak("Nothing matches.");
                    }
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            for (index, entry) in matches.iter().enumerate() {
                                let row = self.row(ui, entry, index == self.selected);
                                if index == self.selected && (down || up) {
                                    row.scroll_to_me(None);
                                }
                                if row.clicked() {
                                    chosen = Some(entry.command.clone());
                                }
                            }
                        });
                });
            });
        let clicked_outside =
            ctx.input(|input| input.pointer.any_pressed()) && !area.response.contains_pointer();
        if chosen.is_some() || enter || clicked_outside {
            self.open = false;
        }
        chosen
    }

    fn row(&self, ui: &mut egui::Ui, entry: &Entry, selected: bool) -> egui::Response {
        let shortcut = shortcut(&entry.command).map(|shortcut| ui.ctx().format_shortcut(&shortcut));
        ui.horizontal(|ui| {
            let label = ui.selectable_label(selected, &entry.label);
            if !entry.detail.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(&entry.detail).weak()).truncate());
            }
            if let Some(shortcut) = shortcut {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(shortcut);
                });
            }
            label
        })
        .inner
    }
}

/// Every character of `query`, in order; higher when they start words or follow each other.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut position = 0;
    let mut previous: Option<usize> = None;
    for wanted in query {
        let found = (position..text.len()).find(|&index| text[index] == wanted)?;
        let word_start = found == 0 || !text[found - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        if previous.is_some_and(|previous| previous + 1 == found) {
            score += 5;
        }
        if previous.is_none() {
            score -= found.min(20) as i32;
        }
        previous = Some(found);
        position = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new("New request", "", Command::NewRequest),
            Entry::new("Client settings…", "", Command::ClientSettings),
            Entry::new(
                "List users",
                "users/list",
                Command::OpenRequest("users/list.req.toml".into()),
            ),
            Entry::new(
                "Create user",
                "users/create",
                Command::OpenRequest("users/create.req.toml".into()),
            ),
            Entry::new("Environment: dev", "", Command::UseEnvironment(Some(0))),
        ]
    }

    #[test]
    fn fuzzy_matches_prefer_word_starts_and_runs() {
        assert!(score("cu", "Create user").is_some());
        assert!(score("xyz", "Create user").is_none());
        assert!(score("cs", "Client settings") > score("cs", "Cookies"));
        assert!(score("set", "Settings") > score("set", "Some extra text"));
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn modes_filter_and_rank_the_entries() {
        let entries = entries();
        let mut palette = Palette::default();
        palette.open(Mode::Everything);
        palette.query = "cuser".into();
        let found: Vec<_> = palette
            .matches(&entries)
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(found[0], "Create user");

        palette.open(Mode::Requests);
        let found: Vec<_> = palette
            .matches(&entries)
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(found, ["List users", "Create user"]);

        palette.open(Mode::Environments);
        assert_eq!(palette.matches(&entries).len(), 1);
    }

    fn frame(
        ctx: &egui::Context,
        palette: &mut Palette,
        entries: &[Entry],
        events: Vec<egui::Event>,
    ) -> (Option<Command>, String) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            events,
            ..Default::default()
        };
        let mut command = None;
        let mut output = ctx.run_ui(input, |ui| command = palette.show(ui.ctx(), entries));
        output.textures_delta.clear();
        (command, format!("{:?}", output.shapes))
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn typing_arrows_and_enter_pick_a_command() {
        let ctx = egui::Context::default();
        let entries = entries();
        let mut palette = Palette::default();
        palette.open(Mode::Everything);
        // An area is only sized on its first frame and drawn from the second.
        frame(&ctx, &mut palette, &entries, Vec::new());
        let (_, shapes) = frame(&ctx, &mut palette, &entries, Vec::new());
        assert!(
            shapes.contains("New request") && shapes.contains("users/list"),
            "{shapes}"
        );

        let typed = vec![egui::Event::Text("user".into())];
        let (_, shapes) = frame(&ctx, &mut palette, &entries, typed);
        assert_eq!(palette.query, "user");
        assert!(!shapes.contains("Client settings"), "{shapes}");

        let (command, _) = frame(
            &ctx,
            &mut palette,
            &entries,
            vec![key(Key::ArrowDown), key(Key::Enter)],
        );
        assert_eq!(
            command,
            Some(Command::OpenRequest("users/create.req.toml".into()))
        );
        assert!(!palette.is_open());

        palette.open(Mode::Everything);
        let (command, _) = frame(&ctx, &mut palette, &entries, vec![key(Key::Escape)]);
        assert_eq!(command, None);
        assert!(!palette.is_open());
    }
}
