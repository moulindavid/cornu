use cornu_core::{KeyValue, Segment, Variables, is_dynamic, segments};
use eframe::egui;

pub fn key_value_table(ui: &mut egui::Ui, id: egui::Id, rows: &mut Vec<KeyValue>) {
    table(ui, id, rows, false);
}

/// A key/value table where each value can be marked secret (kept out of git, masked here).
pub fn variable_table(ui: &mut egui::Ui, id: egui::Id, rows: &mut Vec<KeyValue>) {
    table(ui, id, rows, true);
}

fn table(ui: &mut egui::Ui, id: egui::Id, rows: &mut Vec<KeyValue>, secrets: bool) {
    let mut widths = column_widths(ui.available_width());
    if secrets {
        widths.1 = (widths.1 - 60.0).max(160.0);
    }
    let mut remove = None;
    egui::Grid::new(id)
        .num_columns(if secrets { 5 } else { 4 })
        .striped(true)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            for (index, row) in rows.iter_mut().enumerate() {
                ui.checkbox(&mut row.enabled, "");
                let fields = Fields {
                    id,
                    index,
                    widths,
                    password: row.secret,
                };
                fields.show(ui, &mut row.key, &mut row.value);
                if secrets {
                    let hint = if row.secret {
                        "Secret: stored in .cornu/, outside git"
                    } else {
                        "Mark as secret"
                    };
                    if ui
                        .selectable_label(row.secret, "🔒")
                        .on_hover_text(hint)
                        .clicked()
                    {
                        row.secret = !row.secret;
                    }
                }
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }

            // The empty trailing row shares ids with the row it becomes, so focus survives.
            let (mut key, mut value) = (String::new(), String::new());
            ui.label("");
            let fields = Fields {
                id,
                index: rows.len(),
                widths,
                password: false,
            };
            if fields.show(ui, &mut key, &mut value) {
                rows.push(KeyValue::new(key, value));
            }
            ui.end_row();
        });
    if let Some(index) = remove {
        rows.remove(index);
    }
}

struct Fields {
    id: egui::Id,
    index: usize,
    widths: (f32, f32),
    password: bool,
}

impl Fields {
    fn show(self, ui: &mut egui::Ui, key: &mut String, value: &mut String) -> bool {
        let height = ui.spacing().interact_size.y;
        let key = ui.add_sized(
            [self.widths.0, height],
            egui::TextEdit::singleline(key)
                .id(self.id.with((self.index, "key")))
                .hint_text("Key"),
        );
        let value = ui.add_sized(
            [self.widths.1, height],
            egui::TextEdit::singleline(value)
                .id(self.id.with((self.index, "value")))
                .hint_text("Value")
                .password(self.password),
        );
        key.changed() || value.changed()
    }
}

/// Grid cells only offer last frame's column width, so fields must be sized explicitly.
pub fn column_widths(available: f32) -> (f32, f32) {
    let reserved = 90.0;
    let key = (available * 0.3).max(120.0);
    let value = (available - key - reserved).max(160.0);
    (key, value)
}

pub fn count_label(name: &str, rows: &[KeyValue]) -> String {
    match rows.iter().filter(|row| row.enabled).count() {
        0 => name.to_owned(),
        count => format!("{name} ({count})"),
    }
}

/// egui's `Panel` clamps its first size to the tiny first-frame window, so we split by hand.
pub fn vertical_split(
    ui: &mut egui::Ui,
    id: egui::Id,
    fraction: &mut f32,
    top: impl FnOnce(&mut egui::Ui),
    bottom: impl FnOnce(&mut egui::Ui),
) {
    const HANDLE: f32 = 8.0;
    let rect = ui.available_rect_before_wrap();
    let split_y = rect.top() + rect.height() * *fraction;
    let top_rect =
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), split_y - HANDLE / 2.0));
    let handle_rect = egui::Rect::from_x_y_ranges(
        rect.x_range(),
        egui::Rangef::new(split_y - HANDLE / 2.0, split_y + HANDLE / 2.0),
    );
    let bottom_rect =
        egui::Rect::from_min_max(egui::pos2(rect.left(), split_y + HANDLE / 2.0), rect.max);

    let handle = ui.interact(handle_rect, id, egui::Sense::drag());
    if handle.dragged()
        && let Some(pointer) = handle.interact_pointer_pos()
    {
        *fraction = ((pointer.y - rect.top()) / rect.height()).clamp(0.15, 0.85);
    }
    if handle.hovered() || handle.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    let stroke = if handle.hovered() || handle.dragged() {
        ui.visuals().widgets.hovered.fg_stroke
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    ui.painter().hline(rect.x_range(), split_y, stroke);

    show_in(ui, top_rect, top);
    show_in(ui, bottom_rect, bottom);
    ui.allocate_rect(rect, egui::Sense::hover());
}

fn show_in(ui: &mut egui::Ui, area: egui::Rect, contents: impl FnOnce(&mut egui::Ui)) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
        ui.set_clip_rect(area.intersect(ui.clip_rect()));
        contents(ui);
    });
}

pub fn variable_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    variables: &Variables,
    hint: &str,
) -> egui::Response {
    let font = egui::FontSelection::default().resolve(ui.style());
    let plain = ui.visuals().text_color();
    let (defined, undefined) = variable_colors(ui.visuals());
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let text = buffer.as_str();
        let mut job = egui::text::LayoutJob::default();
        for segment in segments(text) {
            let (part, color) = match segment {
                Segment::Text(part) => (part, plain),
                Segment::Variable { name, span } => {
                    let color = if variables.is_defined(name) {
                        defined
                    } else {
                        undefined
                    };
                    (&text[span], color)
                }
            };
            job.append(part, 0.0, egui::TextFormat::simple(font.clone(), color));
        }
        job.wrap.max_width = wrap_width;
        ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
    };

    let response = ui.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .layouter(&mut layouter),
    );
    let names: Vec<&str> = segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Variable { name, .. } => Some(name),
            Segment::Text(_) => None,
        })
        .collect();
    if names.is_empty() {
        return response;
    }
    response.on_hover_ui(|ui| {
        egui::Grid::new("variable-tooltip")
            .num_columns(2)
            .show(ui, |ui| {
                for name in names {
                    ui.monospace(name);
                    if is_dynamic(name) && variables.get(name).is_none() {
                        ui.colored_label(defined, "generated at send time");
                    } else if let Some(value) = variables.get(name) {
                        ui.colored_label(defined, variables.interpolate(value).text);
                    } else {
                        ui.colored_label(undefined, "not defined");
                    }
                    ui.end_row();
                }
            });
    })
}

fn variable_colors(visuals: &egui::Visuals) -> (egui::Color32, egui::Color32) {
    if visuals.dark_mode {
        (
            egui::Color32::from_rgb(0x7d, 0xc4, 0xe4),
            egui::Color32::from_rgb(0xf3, 0x8b, 0xa8),
        )
    } else {
        (
            egui::Color32::from_rgb(0x1e, 0x66, 0xf5),
            egui::Color32::from_rgb(0xd2, 0x0f, 0x39),
        )
    }
}
