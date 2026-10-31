use std::sync::mpsc::Receiver;

use cornu_core::graphql::{EnumValue, Field, FullType, InputValue, IntrospectionError, Schema};
use cornu_core::graphql::{TypeKind, TypeRef};
use eframe::egui;

pub type Introspection = Receiver<Result<Schema, IntrospectionError>>;

/// The schema of a GraphQL request tab and its explorer, browsed one type at a time.
#[derive(Debug, Default)]
pub struct SchemaPanel {
    schema: Option<Schema>,
    error: Option<String>,
    pending: Option<Introspection>,
    fetch_requested: bool,
    visible: bool,
    /// Visited type names, the last one being shown; empty for the overview.
    path: Vec<String>,
    filter: String,
}

enum Nav {
    Open(String),
    Back(usize),
}

impl SchemaPanel {
    /// The editor cannot send requests itself: the app resolves the request and calls `start`.
    pub fn take_fetch_request(&mut self) -> bool {
        std::mem::take(&mut self.fetch_requested)
    }

    pub fn start(&mut self, pending: Introspection) {
        self.pending = Some(pending);
        self.error = None;
    }

    pub fn shows_explorer(&self) -> bool {
        self.visible && self.schema.is_some()
    }

    fn poll(&mut self) {
        let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.pending = None;
        match result {
            Ok(schema) => self.set_schema(schema),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn set_schema(&mut self, schema: Schema) {
        self.path.retain(|name| schema.type_named(name).is_some());
        self.schema = Some(schema);
        self.visible = true;
    }

    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        self.poll();
        let fetching = self.pending.is_some();
        let label = if self.schema.is_some() {
            "⟳ Refresh schema"
        } else {
            "Fetch schema"
        };
        let button = ui
            .add_enabled(!fetching, egui::Button::new(label).small())
            .on_hover_text("Run an introspection query against this endpoint");
        if button.clicked() {
            self.fetch_requested = true;
        }
        if fetching {
            ui.add(egui::Spinner::new());
        } else if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(schema) = &self.schema {
            let count = schema.types.iter().filter(|ty| !ty.is_builtin()).count();
            ui.toggle_value(&mut self.visible, format!("📖 Docs ({count} types)"));
        }
    }

    pub fn explorer(&mut self, ui: &mut egui::Ui, id: egui::Id, height: f32) {
        let Some(schema) = &self.schema else {
            return;
        };
        let mut nav = None;
        ui.horizontal_wrapped(|ui| {
            if ui.link("Schema").clicked() {
                nav = Some(Nav::Back(0));
            }
            for (depth, name) in self.path.iter().enumerate() {
                ui.weak("›");
                if depth + 1 == self.path.len() {
                    ui.strong(name);
                } else if ui.link(name).clicked() {
                    nav = Some(Nav::Back(depth + 1));
                }
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter")
                .desired_width(f32::INFINITY),
        );
        let filter = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt(id.with("schema-scroll"))
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| match self.path.last() {
                None => overview(ui, schema, &filter, &mut nav),
                Some(name) => match schema.type_named(name) {
                    Some(ty) => type_view(ui, ty, &filter, &mut nav),
                    None => {
                        ui.weak(format!("No type named {name}"));
                    }
                },
            });
        match nav {
            Some(Nav::Open(name)) => {
                if self.path.last() != Some(&name) {
                    self.path.push(name);
                }
                self.filter.clear();
            }
            Some(Nav::Back(depth)) => {
                self.path.truncate(depth);
                self.filter.clear();
            }
            None => {}
        }
    }
}

fn matches(name: &str, filter: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(filter)
}

fn overview(ui: &mut egui::Ui, schema: &Schema, filter: &str, nav: &mut Option<Nav>) {
    if filter.is_empty() {
        for (operation, name) in schema.roots() {
            ui.horizontal(|ui| {
                ui.label(format!("{operation}:"));
                if ui.link(name).clicked() {
                    *nav = Some(Nav::Open(name.to_owned()));
                }
            });
        }
        ui.separator();
    }
    for ty in schema
        .types
        .iter()
        .filter(|ty| !ty.is_builtin() && matches(&ty.name, filter))
    {
        ui.horizontal(|ui| {
            if ui.link(&ty.name).clicked() {
                *nav = Some(Nav::Open(ty.name.clone()));
            }
            ui.weak(ty.kind.label());
        });
    }
}

fn type_view(ui: &mut egui::Ui, ty: &FullType, filter: &str, nav: &mut Option<Nav>) {
    ui.horizontal(|ui| {
        ui.weak(ty.kind.label());
        ui.heading(&ty.name);
    });
    description(ui, &ty.description);
    if !ty.interfaces.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.weak("implements");
            for interface in &ty.interfaces {
                type_link(ui, interface, nav);
            }
        });
    }
    ui.separator();
    match ty.kind {
        TypeKind::Object | TypeKind::Interface => {
            for field in ty.fields.iter().filter(|f| matches(&f.name, filter)) {
                field_row(ui, field, nav);
            }
        }
        TypeKind::InputObject => {
            for input in ty.input_fields.iter().filter(|f| matches(&f.name, filter)) {
                ui.horizontal_wrapped(|ui| input_value(ui, input, nav));
                description(ui, &input.description);
            }
        }
        TypeKind::Enum => {
            for value in ty.enum_values.iter().filter(|v| matches(&v.name, filter)) {
                enum_row(ui, value);
            }
        }
        TypeKind::Union => {
            for member in ty
                .possible_types
                .iter()
                .filter(|t| matches(t.named(), filter))
            {
                type_link(ui, member, nav);
            }
        }
        TypeKind::Scalar | TypeKind::List | TypeKind::NonNull => {}
    }
    if ty.kind == TypeKind::Interface && !ty.possible_types.is_empty() {
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.weak("implemented by");
            for member in &ty.possible_types {
                type_link(ui, member, nav);
            }
        });
    }
}

fn field_row(ui: &mut egui::Ui, field: &Field, nav: &mut Option<Nav>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        name_label(ui, &field.name, field.is_deprecated);
        if !field.args.is_empty() {
            ui.label("(");
            for (index, arg) in field.args.iter().enumerate() {
                if index > 0 {
                    ui.label(", ");
                }
                input_value(ui, arg, nav);
            }
            ui.label(")");
        }
        ui.label(": ");
        type_link(ui, &field.ty, nav);
    });
    deprecation(ui, field.is_deprecated, &field.deprecation_reason);
    description(ui, &field.description);
}

fn input_value(ui: &mut egui::Ui, input: &InputValue, nav: &mut Option<Nav>) {
    ui.label(format!("{}: ", input.name));
    type_link(ui, &input.ty, nav);
    if let Some(default) = &input.default_value {
        ui.weak(format!(" = {default}"));
    }
}

fn enum_row(ui: &mut egui::Ui, value: &EnumValue) {
    name_label(ui, &value.name, value.is_deprecated);
    deprecation(ui, value.is_deprecated, &value.deprecation_reason);
    description(ui, &value.description);
}

fn name_label(ui: &mut egui::Ui, name: &str, deprecated: bool) {
    let mut text = egui::RichText::new(name).monospace().strong();
    if deprecated {
        text = text.strikethrough();
    }
    ui.label(text);
}

fn type_link(ui: &mut egui::Ui, ty: &TypeRef, nav: &mut Option<Nav>) {
    let link = ui.link(egui::RichText::new(ty.to_string()).monospace());
    if link.clicked() {
        *nav = Some(Nav::Open(ty.named().to_owned()));
    }
}

fn deprecation(ui: &mut egui::Ui, deprecated: bool, reason: &Option<String>) {
    if deprecated {
        let reason = reason.as_deref().unwrap_or("no reason given");
        ui.colored_label(ui.visuals().warn_fg_color, format!("Deprecated: {reason}"));
    }
}

fn description(ui: &mut egui::Ui, description: &Option<String>) {
    if let Some(text) = description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        ui.weak(text.trim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTRIES: &str =
        include_str!("../../cornu-core/tests/fixtures/countries-introspection.json");

    fn render(panel: &mut SchemaPanel) -> String {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            panel.toolbar(ui);
            panel.explorer(ui, egui::Id::unique("schema"), 2000.0);
        });
        output.textures_delta.clear();
        format!("{:?}", output.shapes)
    }

    fn loaded() -> SchemaPanel {
        let mut panel = SchemaPanel::default();
        let (tx, rx) = std::sync::mpsc::channel();
        panel.start(rx);
        tx.send(Ok(Schema::parse(COUNTRIES).unwrap())).unwrap();
        panel
    }

    #[test]
    fn fetched_schema_opens_the_overview() {
        let mut panel = loaded();
        let text = render(&mut panel);
        assert!(panel.shows_explorer());
        assert!(
            text.contains("query:") && text.contains("Continent"),
            "{text}"
        );
        assert!(text.contains("Docs (15 types)"), "{text}");
        assert!(!text.contains("__Schema"), "{text}");
    }

    #[test]
    fn types_show_their_fields_and_filter() {
        let mut panel = loaded();
        panel.path = vec!["Query".into(), "Country".into()];
        let text = render(&mut panel);
        assert!(text.contains("Query") && text.contains("Country"), "{text}");
        assert!(
            text.contains("languages") && text.contains("[Language!]!"),
            "{text}"
        );

        panel.filter = "CAPI".into();
        let text = render(&mut panel);
        assert!(
            text.contains("capital") && !text.contains("languages"),
            "{text}"
        );

        panel.path = vec!["Query".into()];
        panel.filter.clear();
        let text = render(&mut panel);
        assert!(text.contains("code") && text.contains("ID!"), "{text}");
    }

    #[test]
    fn failures_are_shown_and_refetch_keeps_the_position() {
        let mut panel = SchemaPanel::default();
        let (tx, rx) = std::sync::mpsc::channel();
        panel.start(rx);
        tx.send(Err(IntrospectionError::Status(401))).unwrap();
        assert!(render(&mut panel).contains("401"));
        assert!(!panel.shows_explorer());

        let mut panel = loaded();
        render(&mut panel);
        panel.path = vec!["Country".into(), "Gone".into()];
        let (tx, rx) = std::sync::mpsc::channel();
        panel.start(rx);
        tx.send(Ok(Schema::parse(COUNTRIES).unwrap())).unwrap();
        render(&mut panel);
        assert_eq!(panel.path, ["Country"]);
    }
}
