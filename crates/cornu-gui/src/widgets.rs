use cornu_core::KeyValue;
use eframe::egui;

pub fn key_value_table(ui: &mut egui::Ui, id: egui::Id, rows: &mut Vec<KeyValue>) {
    let widths = column_widths(ui.available_width());
    let mut remove = None;
    egui::Grid::new(id)
        .num_columns(4)
        .striped(true)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            for (index, row) in rows.iter_mut().enumerate() {
                ui.checkbox(&mut row.enabled, "");
                key_value_fields(ui, id, index, widths, &mut row.key, &mut row.value);
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }

            // The empty trailing row shares ids with the row it becomes, so focus survives.
            let (mut key, mut value) = (String::new(), String::new());
            ui.label("");
            if key_value_fields(ui, id, rows.len(), widths, &mut key, &mut value) {
                rows.push(KeyValue::new(key, value));
            }
            ui.end_row();
        });
    if let Some(index) = remove {
        rows.remove(index);
    }
}

fn key_value_fields(
    ui: &mut egui::Ui,
    id: egui::Id,
    index: usize,
    (key_width, value_width): (f32, f32),
    key: &mut String,
    value: &mut String,
) -> bool {
    let height = ui.spacing().interact_size.y;
    let key = ui.add_sized(
        [key_width, height],
        egui::TextEdit::singleline(key)
            .id(id.with((index, "key")))
            .hint_text("Key"),
    );
    let value = ui.add_sized(
        [value_width, height],
        egui::TextEdit::singleline(value)
            .id(id.with((index, "value")))
            .hint_text("Value"),
    );
    key.changed() || value.changed()
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
