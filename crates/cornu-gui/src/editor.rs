use std::path::PathBuf;

use cornu_core::{ApiKeyLocation, Auth, Body, KeyValue, MultipartPart, PartValue, Request};
use eframe::egui;

use crate::widgets::{column_widths, count_label, key_value_table};

const RAW_CONTENT_TYPES: [&str; 6] = [
    "text/plain",
    "application/xml",
    "text/html",
    "text/csv",
    "application/javascript",
    "application/yaml",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Tab {
    #[default]
    Params,
    Headers,
    Body,
    Auth,
}

#[derive(Debug, Default)]
pub struct EditorState {
    tab: Tab,
    json_error: Option<String>,
}

pub fn request_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    request: &mut Request,
    state: &mut EditorState,
) {
    ui.horizontal(|ui| {
        let params = count_label("Params", &request.query);
        let headers = count_label("Headers", &request.headers);
        let body = match body_kind(&request.body) {
            BodyKind::None => "Body".to_owned(),
            kind => format!("Body ({})", kind.label()),
        };
        let auth = match auth_kind(&request.auth) {
            AuthKind::Inherit => "Auth".to_owned(),
            kind => format!("Auth ({})", kind.label()),
        };
        ui.selectable_value(&mut state.tab, Tab::Params, params);
        ui.selectable_value(&mut state.tab, Tab::Headers, headers);
        ui.selectable_value(&mut state.tab, Tab::Body, body);
        ui.selectable_value(&mut state.tab, Tab::Auth, auth);
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt(id.with("editor-scroll"))
        .show(ui, |ui| match state.tab {
            Tab::Params => key_value_table(ui, id.with("params"), &mut request.query),
            Tab::Headers => key_value_table(ui, id.with("headers"), &mut request.headers),
            Tab::Body => body_editor(ui, id.with("body"), &mut request.body, state),
            Tab::Auth => auth_editor(ui, &mut request.auth),
        });
}

/// Moves `?a=1&b=2` from the URL into the query table, decoding percent escapes.
pub fn extract_query(url: &mut String, query: &mut Vec<KeyValue>) -> bool {
    let Some((base, rest)) = url.split_once('?') else {
        return false;
    };
    let (query_string, fragment) = match rest.split_once('#') {
        Some((query_string, fragment)) => (query_string, Some(fragment)),
        None => (rest, None),
    };
    query.extend(
        url::form_urlencoded::parse(query_string.as_bytes())
            .map(|(key, value)| KeyValue::new(key, value)),
    );
    let mut new_url = base.to_owned();
    if let Some(fragment) = fragment {
        new_url.push('#');
        new_url.push_str(fragment);
    }
    *url = new_url;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    None,
    Raw,
    Json,
    Form,
    Multipart,
    Binary,
}

impl BodyKind {
    const ALL: [BodyKind; 6] = [
        BodyKind::None,
        BodyKind::Raw,
        BodyKind::Json,
        BodyKind::Form,
        BodyKind::Multipart,
        BodyKind::Binary,
    ];

    fn label(self) -> &'static str {
        match self {
            BodyKind::None => "None",
            BodyKind::Raw => "Raw",
            BodyKind::Json => "JSON",
            BodyKind::Form => "Form URL-encoded",
            BodyKind::Multipart => "Multipart",
            BodyKind::Binary => "Binary file",
        }
    }
}

fn body_kind(body: &Body) -> BodyKind {
    match body {
        Body::None => BodyKind::None,
        Body::Raw { .. } => BodyKind::Raw,
        Body::Json { .. } => BodyKind::Json,
        Body::FormUrlEncoded { .. } => BodyKind::Form,
        Body::Multipart { .. } => BodyKind::Multipart,
        Body::Binary { .. } => BodyKind::Binary,
    }
}

fn convert_body(body: &Body, kind: BodyKind) -> Body {
    let text = match body {
        Body::Raw { text, .. } | Body::Json { text } => text.clone(),
        _ => String::new(),
    };
    match kind {
        BodyKind::None => Body::None,
        BodyKind::Raw => Body::Raw {
            text,
            content_type: RAW_CONTENT_TYPES[0].to_owned(),
        },
        BodyKind::Json => Body::Json { text },
        BodyKind::Form => Body::FormUrlEncoded { fields: Vec::new() },
        BodyKind::Multipart => Body::Multipart { parts: Vec::new() },
        BodyKind::Binary => Body::Binary {
            path: PathBuf::new(),
            content_type: None,
        },
    }
}

fn body_editor(ui: &mut egui::Ui, id: egui::Id, body: &mut Body, state: &mut EditorState) {
    let current = body_kind(body);
    ui.horizontal_wrapped(|ui| {
        for kind in BodyKind::ALL {
            if ui.radio(current == kind, kind.label()).clicked() && current != kind {
                *body = convert_body(body, kind);
                state.json_error = None;
            }
        }
    });
    ui.add_space(4.0);

    match body {
        Body::None => {
            ui.weak("This request has no body.");
        }
        Body::Raw { text, content_type } => {
            egui::ComboBox::from_id_salt(id.with("content-type"))
                .selected_text(content_type.as_str())
                .show_ui(ui, |ui| {
                    for preset in RAW_CONTENT_TYPES {
                        ui.selectable_value(content_type, preset.to_owned(), preset);
                    }
                });
            code_area(ui, text);
        }
        Body::Json { text } => {
            ui.horizontal(|ui| {
                if ui.button("Format").clicked() {
                    state.json_error = format_json(text).err();
                }
                if let Some(error) = &state.json_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
            if code_area(ui, text).changed() {
                state.json_error = None;
            }
        }
        Body::FormUrlEncoded { fields } => key_value_table(ui, id.with("form"), fields),
        Body::Multipart { parts } => multipart_editor(ui, id.with("multipart"), parts),
        Body::Binary { path, .. } => {
            ui.horizontal(|ui| {
                path_picker(ui, path, f32::INFINITY);
            });
        }
    }
}

fn code_area(ui: &mut egui::Ui, text: &mut String) -> egui::Response {
    ui.add(
        egui::TextEdit::multiline(text)
            .code_editor()
            .desired_rows(12)
            .desired_width(f32::INFINITY),
    )
}

fn format_json(text: &mut String) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("Invalid JSON: {error}"))?;
    *text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    Ok(())
}

fn multipart_editor(ui: &mut egui::Ui, id: egui::Id, parts: &mut Vec<MultipartPart>) {
    let (name_width, value_width) = column_widths(ui.available_width() - 80.0);
    let height = ui.spacing().interact_size.y;
    let mut remove = None;
    egui::Grid::new(id)
        .num_columns(5)
        .striped(true)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            for (index, part) in parts.iter_mut().enumerate() {
                ui.checkbox(&mut part.enabled, "");
                ui.add_sized(
                    [name_width, height],
                    egui::TextEdit::singleline(&mut part.name).hint_text("Name"),
                );
                let is_file = matches!(part.value, PartValue::File(_));
                egui::ComboBox::from_id_salt(id.with((index, "kind")))
                    .selected_text(if is_file { "File" } else { "Text" })
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(!is_file, "Text").clicked() && is_file {
                            part.value = PartValue::Text(String::new());
                        }
                        if ui.selectable_label(is_file, "File").clicked() && !is_file {
                            part.value = PartValue::File(PathBuf::new());
                        }
                    });
                ui.horizontal(|ui| match &mut part.value {
                    PartValue::Text(text) => {
                        ui.add_sized(
                            [value_width, height],
                            egui::TextEdit::singleline(text).hint_text("Value"),
                        );
                    }
                    PartValue::File(path) => path_picker(ui, path, value_width - 80.0),
                });
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        parts.remove(index);
    }
    if ui.button("+ Add part").clicked() {
        parts.push(MultipartPart::text("", ""));
    }
}

fn path_picker(ui: &mut egui::Ui, path: &mut PathBuf, width: f32) {
    if ui.button("Browse…").clicked()
        && let Some(picked) = rfd::FileDialog::new().pick_file()
    {
        *path = picked;
    }
    let mut text = path.to_string_lossy().into_owned();
    let field = egui::TextEdit::singleline(&mut text).hint_text("No file selected");
    let response = if width.is_finite() {
        ui.add_sized([width, ui.spacing().interact_size.y], field)
    } else {
        ui.add(field.desired_width(width))
    };
    if response.changed() {
        *path = PathBuf::from(text);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    Inherit,
    None,
    Bearer,
    Basic,
    ApiKey,
}

impl AuthKind {
    const ALL: [AuthKind; 5] = [
        AuthKind::Inherit,
        AuthKind::None,
        AuthKind::Bearer,
        AuthKind::Basic,
        AuthKind::ApiKey,
    ];

    fn label(self) -> &'static str {
        match self {
            AuthKind::Inherit => "Inherit",
            AuthKind::None => "None",
            AuthKind::Bearer => "Bearer token",
            AuthKind::Basic => "Basic",
            AuthKind::ApiKey => "API key",
        }
    }

    fn default_auth(self) -> Auth {
        match self {
            AuthKind::Inherit => Auth::Inherit,
            AuthKind::None => Auth::None,
            AuthKind::Bearer => Auth::Bearer {
                token: String::new(),
            },
            AuthKind::Basic => Auth::Basic {
                username: String::new(),
                password: String::new(),
            },
            AuthKind::ApiKey => Auth::ApiKey {
                key: String::new(),
                value: String::new(),
                location: ApiKeyLocation::Header,
            },
        }
    }
}

fn auth_kind(auth: &Auth) -> AuthKind {
    match auth {
        Auth::Inherit => AuthKind::Inherit,
        Auth::None => AuthKind::None,
        Auth::Bearer { .. } => AuthKind::Bearer,
        Auth::Basic { .. } => AuthKind::Basic,
        Auth::ApiKey { .. } => AuthKind::ApiKey,
    }
}

pub fn auth_editor(ui: &mut egui::Ui, auth: &mut Auth) {
    let current = auth_kind(auth);
    egui::ComboBox::from_label("Type")
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for kind in AuthKind::ALL {
                if ui.selectable_label(current == kind, kind.label()).clicked() && current != kind {
                    *auth = kind.default_auth();
                }
            }
        });
    ui.add_space(6.0);

    egui::Grid::new("auth-fields")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| match auth {
            Auth::Inherit => {
                ui.weak("Uses the auth of the parent folder or collection.");
                ui.end_row();
            }
            Auth::None => {
                ui.weak("No auth is sent, even if a parent folder defines one.");
                ui.end_row();
            }
            Auth::Bearer { token } => {
                field(ui, "Token", token, "{{token}}", false);
            }
            Auth::Basic { username, password } => {
                field(ui, "Username", username, "", false);
                field(ui, "Password", password, "", true);
            }
            Auth::ApiKey {
                key,
                value,
                location,
            } => {
                field(ui, "Key", key, "X-Api-Key", false);
                field(ui, "Value", value, "{{api_key}}", false);
                ui.label("Add to");
                ui.horizontal(|ui| {
                    ui.radio_value(location, ApiKeyLocation::Header, "Header");
                    ui.radio_value(location, ApiKeyLocation::Query, "Query params");
                });
                ui.end_row();
            }
        });
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, password: bool) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .password(password)
            .desired_width(320.0),
    );
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_query_moves_and_decodes_pairs() {
        let mut url = "https://api.example/users?page=2&q=a%20b&flag#top".to_owned();
        let mut query = vec![KeyValue::new("existing", "1")];
        assert!(extract_query(&mut url, &mut query));
        assert_eq!(url, "https://api.example/users#top");
        let pairs: Vec<_> = query
            .iter()
            .map(|row| (row.key.as_str(), row.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("existing", "1"), ("page", "2"), ("q", "a b"), ("flag", "")]
        );
    }

    #[test]
    fn extract_query_keeps_variables_and_ignores_plain_urls() {
        let mut url = "{{base}}/search?term={{term}}".to_owned();
        let mut query = Vec::new();
        assert!(extract_query(&mut url, &mut query));
        assert_eq!(url, "{{base}}/search");
        assert_eq!(query[0].value, "{{term}}");

        let mut plain = "https://example.com".to_owned();
        assert!(!extract_query(&mut plain, &mut query));
    }

    #[test]
    fn switching_between_raw_and_json_keeps_text() {
        let json = Body::Json {
            text: "{\"a\":1}".into(),
        };
        let raw = convert_body(&json, BodyKind::Raw);
        assert!(matches!(&raw, Body::Raw { text, .. } if text == "{\"a\":1}"));
        assert_eq!(convert_body(&raw, BodyKind::Json), json);
    }

    #[test]
    fn format_json_pretty_prints_or_reports() {
        let mut text = r#"{"a":[1,2]}"#.to_owned();
        format_json(&mut text).unwrap();
        assert_eq!(text, "{\n  \"a\": [\n    1,\n    2\n  ]\n}");

        let mut invalid = r#"{"a": {{value}}}"#.to_owned();
        assert!(format_json(&mut invalid).is_err());
        assert_eq!(invalid, r#"{"a": {{value}}}"#);
    }
}
