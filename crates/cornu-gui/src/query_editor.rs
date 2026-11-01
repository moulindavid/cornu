use cornu_core::graphql::Schema;
use cornu_core::graphql::completion::{Suggestion, complete};
use eframe::egui;
use egui::text::{CCursor, CCursorRange};

const TRIGGER: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Space);

#[derive(Debug, Default)]
pub struct CompletionPopup {
    open: bool,
    selected: usize,
    /// Where the popup was drawn last frame: clicking it takes the focus from the editor.
    rect: Option<egui::Rect>,
}

/// The GraphQL query text area, with schema completions while typing or on Ctrl+Space.
pub fn query_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    query: &mut String,
    schema: Option<&Schema>,
    popup: &mut CompletionPopup,
) {
    let focused = ui.memory(|memory| memory.has_focus(id));
    let mut moved = false;
    let mut accept = None;
    if popup.open && focused {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                popup.selected += 1;
                moved = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                popup.selected = popup.selected.saturating_sub(1);
                moved = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
            {
                accept = Some(popup.selected);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                popup.open = false;
            }
        });
    }
    let triggered = focused && ui.input_mut(|input| input.consume_shortcut(&TRIGGER));

    let output = egui::TextEdit::multiline(query)
        .id(id)
        .code_editor()
        .desired_rows(12)
        .desired_width(f32::INFINITY)
        .show(ui);
    let Some(schema) = schema else {
        popup.open = false;
        return;
    };
    let cursor = output
        .cursor_range
        .or_else(|| output.state.cursor.char_range())
        .map(|range| range.primary.index.0);
    let over_popup = popup
        .rect
        .is_some_and(|rect| ui.rect_contains_pointer(rect));
    if !output.response.has_focus() && !over_popup {
        popup.open = false;
    } else if triggered {
        popup.open = true;
        popup.selected = 0;
    } else if output.response.changed() {
        popup.open = cursor.is_some_and(|cursor| typing_name(query, cursor));
        popup.selected = 0;
    }
    let Some(cursor) = cursor.filter(|_| popup.open) else {
        popup.open = false;
        popup.rect = None;
        return;
    };

    let byte = byte_offset(query, cursor);
    let completion = complete(schema, query, byte);
    if completion.items.is_empty() {
        popup.open = false;
        popup.rect = None;
        return;
    }
    popup.selected = popup.selected.min(completion.items.len() - 1);

    let caret = output.galley.pos_from_cursor(CCursor::new(cursor));
    let anchor = caret.left_bottom() + output.galley_pos.to_vec2();
    let area = egui::Area::new(id.with("completion"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(240.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (index, item) in completion.items.iter().enumerate() {
                            let row = suggestion_row(ui, item, index == popup.selected);
                            if moved && index == popup.selected {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                accept = Some(index);
                            }
                        }
                    });
            });
        });
    popup.rect = Some(area.response.rect);

    if let Some(item) = accept.and_then(|index| completion.items.get(index)) {
        query.replace_range(completion.start..byte, &item.insert);
        let start = query[..completion.start].chars().count();
        let end = start + item.insert.chars().count();
        let mut state = output.state;
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(end))));
        state.store(ui.ctx(), id);
        ui.memory_mut(|memory| memory.request_focus(id));
        // After `arg: ` the enum values, if any, are the next thing to pick.
        popup.open = item.insert.ends_with(": ");
        popup.selected = 0;
    }
}

fn suggestion_row(ui: &mut egui::Ui, item: &Suggestion, selected: bool) -> egui::Response {
    let mut label = egui::RichText::new(&item.label).monospace();
    if item.deprecated {
        label = label.strikethrough();
    }
    let row = ui
        .horizontal(|ui| {
            let row = ui.selectable_label(selected, label);
            ui.weak(egui::RichText::new(&item.detail).monospace());
            row
        })
        .inner;
    match item.description.as_deref() {
        Some(description) if !description.trim().is_empty() => {
            row.on_hover_text(description.trim())
        }
        _ => row,
    }
}

fn typing_name(text: &str, cursor: usize) -> bool {
    text.chars()
        .nth(cursor.wrapping_sub(1))
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn byte_offset(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTRIES: &str =
        include_str!("../../cornu-core/tests/fixtures/countries-introspection.json");

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    struct Harness {
        ctx: egui::Context,
        id: egui::Id,
        schema: Schema,
        query: String,
        popup: CompletionPopup,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                id: egui::Id::unique("query"),
                schema: Schema::parse(COUNTRIES).unwrap(),
                query: String::new(),
                popup: CompletionPopup::default(),
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> String {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut output = self.ctx.run_ui(input, |ui| {
                ui.memory_mut(|memory| memory.request_focus(self.id));
                query_editor(
                    ui,
                    self.id,
                    &mut self.query,
                    Some(&self.schema),
                    &mut self.popup,
                );
            });
            output.textures_delta.clear();
            format!("{:?}", output.shapes)
        }
    }

    #[test]
    fn typing_shows_fields_and_enter_inserts_the_selection() {
        let mut harness = Harness::new();
        harness.frame(Vec::new());
        harness.frame(vec![egui::Event::Text("{ cou".into())]);
        let shapes = harness.frame(Vec::new());
        assert!(harness.popup.open);
        assert!(shapes.contains("countries") && shapes.contains("[Country!]!"));

        harness.frame(vec![key(egui::Key::ArrowDown)]);
        harness.frame(vec![key(egui::Key::Enter)]);
        assert_eq!(harness.query, "{ country");
        assert!(!harness.popup.open);

        harness.frame(vec![egui::Event::Text("(".into())]);
        assert!(!harness.popup.open, "only names open the popup");
        harness.frame(vec![key(egui::Key::Tab)]);
        assert_eq!(
            harness.query, "{ country(\t",
            "closed popup leaves Tab alone"
        );
    }

    #[test]
    fn ctrl_space_opens_and_escape_closes() {
        let mut harness = Harness::new();
        harness.query = "{ countries { languages {  } } }".into();
        harness.frame(Vec::new());
        let mut state = egui::TextEdit::load_state(&harness.ctx, harness.id).unwrap();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(26))));
        state.store(&harness.ctx, harness.id);
        harness.frame(vec![egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }]);
        let shapes = harness.frame(Vec::new());
        assert!(harness.popup.open && shapes.contains("native"), "{shapes}");

        harness.frame(vec![key(egui::Key::Escape)]);
        assert!(!harness.popup.open);
        assert_eq!(harness.query, "{ countries { languages {  } } }");
    }

    #[test]
    fn char_cursors_map_to_bytes() {
        assert_eq!(byte_offset("é{ a", 2), 3);
        assert_eq!(byte_offset("ab", 5), 2);
        assert!(typing_name("{ a", 3));
        assert!(!typing_name("{ a(", 4));
        assert!(!typing_name("", 0));
    }
}
