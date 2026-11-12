use std::path::PathBuf;

use cornu_core::{
    ApiKeyLocation, AssertSource, Assertion, Auth, Body, Capture, CaptureSource, ClientAuth,
    DEFAULT_REDIRECT_URI, Grant, KeyValue, Method, MultipartPart, OAuth2, Operator, PartValue,
    Request, Scripts,
};
use eframe::egui;

use crate::oauth::TokenPanel;
use crate::query_editor::{CompletionPopup, query_editor};
use crate::schema::SchemaPanel;
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
    Captures,
    Tests,
    Scripts,
}

#[derive(Debug, Default)]
pub struct EditorState {
    tab: Tab,
    json_error: Option<String>,
    pub schema: SchemaPanel,
    completion: CompletionPopup,
    pub token: TokenPanel,
    /// Messages are composed next to the log, and captures and tests do not apply.
    pub websocket: bool,
    /// The body is the JSON message and the query does not apply.
    pub grpc: bool,
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
        if state.websocket
            && matches!(
                state.tab,
                Tab::Body | Tab::Captures | Tab::Tests | Tab::Scripts
            )
        {
            state.tab = Tab::Params;
        }
        if state.grpc && state.tab == Tab::Params {
            state.tab = Tab::Body;
        }
        if !state.grpc {
            ui.selectable_value(&mut state.tab, Tab::Params, params);
        }
        if state.grpc {
            ui.selectable_value(&mut state.tab, Tab::Body, "Message");
        }
        ui.selectable_value(&mut state.tab, Tab::Headers, headers);
        if !state.websocket && !state.grpc {
            ui.selectable_value(&mut state.tab, Tab::Body, body);
        }
        ui.selectable_value(&mut state.tab, Tab::Auth, auth);
        if state.websocket {
            return;
        }
        let captures = match request.captures.iter().filter(|c| c.enabled).count() {
            0 => "Captures".to_owned(),
            count => format!("Captures ({count})"),
        };
        ui.selectable_value(&mut state.tab, Tab::Captures, captures);
        let tests = match request.assertions.iter().filter(|a| a.enabled).count() {
            0 => "Tests".to_owned(),
            count => format!("Tests ({count})"),
        };
        ui.selectable_value(&mut state.tab, Tab::Tests, tests);
        ui.selectable_value(
            &mut state.tab,
            Tab::Scripts,
            scripts_label(&request.scripts),
        );
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt(id.with("editor-scroll"))
        .show(ui, |ui| match state.tab {
            Tab::Params => key_value_table(ui, id.with("params"), &mut request.query),
            Tab::Headers => key_value_table(ui, id.with("headers"), &mut request.headers),
            Tab::Body if state.grpc => message_editor(ui, &mut request.body, state),
            Tab::Body => {
                let before = body_kind(&request.body);
                body_editor(ui, id.with("body"), &mut request.body, state);
                // GraphQL servers expect POST; a GET left over from a new draft would be rejected.
                if before != BodyKind::GraphQl
                    && body_kind(&request.body) == BodyKind::GraphQl
                    && request.method == Method::Get
                {
                    request.method = Method::Post;
                }
            }
            Tab::Auth => auth_editor(ui, &mut request.auth, &mut state.token),
            Tab::Captures => capture_table(ui, id.with("captures"), &mut request.captures),
            Tab::Tests => assertion_table(ui, id.with("tests"), &mut request.assertions),
            Tab::Scripts => scripts_editor(ui, &mut request.scripts),
        });
}

fn assertion_table(ui: &mut egui::Ui, id: egui::Id, assertions: &mut Vec<Assertion>) {
    ui.weak("Checked on every response. With tests, a request passes when they all do, whatever its status.");
    ui.add_space(4.0);
    let width = ((ui.available_width() - 330.0) / 2.0).max(120.0);
    let mut remove = None;
    egui::Grid::new(id)
        .num_columns(6)
        .striped(true)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            for (index, assertion) in assertions.iter_mut().enumerate() {
                ui.checkbox(&mut assertion.enabled, "");
                egui::ComboBox::from_id_salt(id.with(("source", index)))
                    .selected_text(assertion.source.label())
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for source in AssertSource::ALL {
                            ui.selectable_value(&mut assertion.source, source, source.label());
                        }
                    });
                let hint = match assertion.source {
                    AssertSource::Body => "$.data.id (empty: whole body)",
                    AssertSource::Header => "Header name",
                    _ => "",
                };
                ui.add_enabled(
                    assertion.source.takes_expression(),
                    egui::TextEdit::singleline(&mut assertion.expression)
                        .id(id.with(("expression", index)))
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(width),
                );
                egui::ComboBox::from_id_salt(id.with(("operator", index)))
                    .selected_text(assertion.operator.label())
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for operator in Operator::ALL {
                            ui.selectable_value(
                                &mut assertion.operator,
                                operator,
                                operator.label(),
                            );
                        }
                    });
                ui.add_enabled(
                    assertion.operator.takes_value(),
                    egui::TextEdit::singleline(&mut assertion.expected)
                        .id(id.with(("expected", index)))
                        .hint_text("expected")
                        .desired_width(width),
                );
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        assertions.remove(index);
    }
    if ui.button("➕ Add test").clicked() {
        let assertion = if assertions.is_empty() {
            Assertion::status(200)
        } else {
            Assertion::body("", Operator::Equals, "")
        };
        assertions.push(assertion);
    }
}

fn capture_table(ui: &mut egui::Ui, id: egui::Id, captures: &mut Vec<Capture>) {
    ui.weak("Store values of the response into variables, for the requests sent after this one.");
    ui.add_space(4.0);
    let (name_width, expression_width) = column_widths(ui.available_width() - 90.0);
    let mut remove = None;
    egui::Grid::new(id)
        .num_columns(5)
        .striped(true)
        .spacing([6.0, 4.0])
        .show(ui, |ui| {
            for (index, capture) in captures.iter_mut().enumerate() {
                ui.checkbox(&mut capture.enabled, "");
                ui.add(
                    egui::TextEdit::singleline(&mut capture.variable)
                        .id(id.with(("variable", index)))
                        .hint_text("variable")
                        .desired_width(name_width),
                );
                egui::ComboBox::from_id_salt(id.with(("source", index)))
                    .selected_text(capture.source.label())
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for source in CaptureSource::ALL {
                            ui.selectable_value(&mut capture.source, source, source.label());
                        }
                    });
                let hint = match capture.source {
                    CaptureSource::Body => "$.data.token (empty: whole body)",
                    CaptureSource::Header => "Header name",
                    CaptureSource::Status => "",
                };
                ui.add_enabled(
                    capture.source != CaptureSource::Status,
                    egui::TextEdit::singleline(&mut capture.expression)
                        .id(id.with(("expression", index)))
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(expression_width),
                );
                if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        captures.remove(index);
    }
    if ui.button("➕ Add capture").clicked() {
        captures.push(Capture::body("", ""));
    }
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
    GraphQl,
}

impl BodyKind {
    const ALL: [BodyKind; 7] = [
        BodyKind::None,
        BodyKind::Raw,
        BodyKind::Json,
        BodyKind::GraphQl,
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
            BodyKind::GraphQl => "GraphQL",
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
        Body::GraphQl { .. } => BodyKind::GraphQl,
    }
}

fn convert_body(body: &Body, kind: BodyKind) -> Body {
    let text = match body {
        Body::Raw { text, .. } | Body::Json { text } => text.clone(),
        Body::GraphQl { query, variables } => Body::graphql_payload(query, variables)
            .ok()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|payload| serde_json::to_string_pretty(&payload).ok())
            .unwrap_or_default(),
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
        BodyKind::GraphQl => Body::graphql_from_payload(&text).unwrap_or(Body::GraphQl {
            query: String::new(),
            variables: String::new(),
        }),
    }
}

fn message_editor(ui: &mut egui::Ui, body: &mut Body, state: &mut EditorState) {
    let mut text = crate::websocket::message(body).to_owned();
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.button("Format").clicked() {
            state.json_error = format_json(&mut text).err();
            changed = true;
        }
        ui.weak("The input message as JSON, an array of messages for client streaming.");
        if let Some(error) = &state.json_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
    if code_area(ui, &mut text).changed() {
        state.json_error = None;
        changed = true;
    }
    if changed {
        crate::websocket::set_message(body, text);
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
        Body::GraphQl { query, variables } => graphql_editor(ui, id, query, variables, state),
    }
}

fn graphql_editor(
    ui: &mut egui::Ui,
    id: egui::Id,
    query: &mut String,
    variables: &mut String,
    state: &mut EditorState,
) {
    ui.horizontal(|ui| state.schema.toolbar(ui));
    let explorer = state.schema.shows_explorer();
    ui.columns(if explorer { 3 } else { 2 }, |columns| {
        if explorer {
            let height = columns[0].spacing().interact_size.y * 12.0;
            state
                .schema
                .explorer(&mut columns[2], id.with("schema"), height);
        }
        columns[0].horizontal(|ui| {
            ui.label("Query");
            if state.schema.schema().is_some() {
                ui.weak("Ctrl+Space for suggestions");
            }
        });
        query_editor(
            &mut columns[0],
            id.with("query"),
            query,
            state.schema.schema(),
            &mut state.completion,
        );

        let ui = &mut columns[1];
        ui.horizontal(|ui| {
            ui.label("Variables");
            if ui.small_button("Format").clicked() && !variables.trim().is_empty() {
                state.json_error = format_json(variables).err();
            }
        });
        if code_area(ui, variables).changed() {
            state.json_error = None;
        }
        let error = state
            .json_error
            .clone()
            .or_else(|| variables_problem(variables));
        if let Some(error) = error {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }
    });
}

/// Placeholders such as `{"id": {{id}}}` only become valid JSON once resolved.
fn variables_problem(variables: &str) -> Option<String> {
    if variables.trim().is_empty() || variables.contains("{{") {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(variables) {
        Ok(serde_json::Value::Object(_)) => None,
        Ok(_) => Some("Variables must be a JSON object".into()),
        Err(error) => Some(format!("Invalid JSON: {error}")),
    }
}

pub fn scripts_label(scripts: &Scripts) -> &'static str {
    if scripts.is_empty() {
        "Scripts"
    } else {
        "Scripts ●"
    }
}

/// The Rhai scripts of a request or of a folder, whose scripts run first.
pub fn scripts_editor(ui: &mut egui::Ui, scripts: &mut Scripts) {
    ui.strong("Pre-request");
    ui.weak(
        "Runs before variables are resolved: change request.method, url, headers, query or body, \
         set vars.name, and resolve(\"{{name}}\") for a value.",
    );
    script_area(ui, &mut scripts.pre);
    ui.add_space(8.0);
    ui.strong("Post-response");
    ui.weak(
        "Reads response.status, headers, body, json, time and size; set vars.name and add \
         test(\"name\", condition).",
    );
    script_area(ui, &mut scripts.post);
    ui.add_space(4.0);
    egui::CollapsingHeader::new("Helpers")
        .id_salt("script-helpers")
        .show(ui, |ui| {
            for line in [
                "print(value)                     shown in the Console of the response",
                "sha256(text)                     hex digest",
                "hmac_sha256(key, text)           hex signature",
                "hmac_sha256_base64(key, text)    base64 signature",
                "base64_encode(text), base64_decode(text), url_encode(text)",
                "uuid(), timestamp(), timestamp_ms(), now_iso()",
            ] {
                ui.monospace(line);
            }
            ui.hyperlink_to("The Rhai language", "https://rhai.rs/book/language/");
        });
}

fn script_area(ui: &mut egui::Ui, text: &mut String) {
    ui.add(
        egui::TextEdit::multiline(text)
            .code_editor()
            .desired_rows(8)
            .desired_width(f32::INFINITY),
    );
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
    OAuth2,
}

impl AuthKind {
    const ALL: [AuthKind; 6] = [
        AuthKind::Inherit,
        AuthKind::None,
        AuthKind::Bearer,
        AuthKind::Basic,
        AuthKind::ApiKey,
        AuthKind::OAuth2,
    ];

    fn label(self) -> &'static str {
        match self {
            AuthKind::Inherit => "Inherit",
            AuthKind::None => "None",
            AuthKind::Bearer => "Bearer token",
            AuthKind::Basic => "Basic",
            AuthKind::ApiKey => "API key",
            AuthKind::OAuth2 => "OAuth 2.0",
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
            AuthKind::OAuth2 => Auth::OAuth2(OAuth2 {
                client_secret: "{{client_secret}}".into(),
                ..Default::default()
            }),
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
        Auth::OAuth2(_) => AuthKind::OAuth2,
    }
}

pub fn auth_editor(ui: &mut egui::Ui, auth: &mut Auth, token: &mut TokenPanel) {
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
            Auth::OAuth2(config) => oauth2_fields(ui, config),
        });
    token.show(ui);
}

fn oauth2_fields(ui: &mut egui::Ui, config: &mut OAuth2) {
    ui.label("Grant");
    egui::ComboBox::from_id_salt("oauth2-grant")
        .selected_text(config.grant.label())
        .show_ui(ui, |ui| {
            for grant in Grant::ALL {
                ui.selectable_value(&mut config.grant, grant, grant.label());
            }
        });
    ui.end_row();
    let code = config.grant == Grant::AuthorizationCode;
    if code {
        field(
            ui,
            "Authorization URL",
            &mut config.auth_url,
            "https://auth.example.com/authorize",
            false,
        );
    }
    field(
        ui,
        "Token URL",
        &mut config.token_url,
        "https://auth.example.com/oauth/token",
        false,
    );
    field(
        ui,
        "Client ID",
        &mut config.client_id,
        "{{client_id}}",
        false,
    );
    let hint = if code {
        "empty for a public client"
    } else {
        "{{client_secret}}"
    };
    field(ui, "Client secret", &mut config.client_secret, hint, true);
    field(ui, "Scope", &mut config.scope, "read write", false);
    if code {
        field(
            ui,
            "Redirect URI",
            &mut config.redirect_uri,
            DEFAULT_REDIRECT_URI,
            false,
        );
    }
    ui.label("Send credentials");
    ui.horizontal(|ui| {
        ui.radio_value(
            &mut config.client_auth,
            ClientAuth::Header,
            "Basic auth header",
        );
        ui.radio_value(&mut config.client_auth, ClientAuth::Body, "In the body");
    });
    ui.end_row();
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
    fn switching_between_json_and_graphql_keeps_the_payload() {
        let json = Body::Json {
            text: r#"{"query": "{ a }", "variables": {"id": 1}}"#.into(),
        };
        let graphql = convert_body(&json, BodyKind::GraphQl);
        assert_eq!(
            graphql,
            Body::GraphQl {
                query: "{ a }".into(),
                variables: "{\n  \"id\": 1\n}".into()
            }
        );
        let Body::Json { text } = convert_body(&graphql, BodyKind::Json) else {
            panic!("expected json");
        };
        assert_eq!(
            convert_body(&Body::Json { text }, BodyKind::GraphQl),
            graphql
        );
        assert_eq!(
            convert_body(&Body::None, BodyKind::GraphQl),
            Body::GraphQl {
                query: String::new(),
                variables: String::new()
            }
        );
    }

    #[test]
    fn graphql_editor_renders_query_and_variables() {
        let mut request = Request::new(Method::Get, "https://x.example/graphql");
        request.body = Body::GraphQl {
            query: "{ countries { code } }".into(),
            variables: "[1]".into(),
        };
        let mut state = EditorState {
            tab: Tab::Body,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        assert!(
            text.contains("countries") && text.contains("Variables"),
            "{text}"
        );
        assert!(text.contains("must be a JSON object"), "{text}");
        assert!(text.contains("Fetch schema"), "{text}");
    }

    #[test]
    fn graphql_variables_are_checked_unless_templated() {
        assert_eq!(variables_problem(""), None);
        assert_eq!(variables_problem(r#"{"id": {{id}}}"#), None);
        assert_eq!(variables_problem(r#"{"id": 1}"#), None);
        assert!(variables_problem("[1]").is_some());
        assert!(variables_problem("{ nope").is_some());
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

    #[test]
    fn captures_tab_lists_each_capture() {
        let mut request = Request::new(Method::Post, "https://x.example/login")
            .with_capture(Capture::body("token", "$.access_token"))
            .with_capture(Capture::header("location", "Location"));
        let mut state = EditorState {
            tab: Tab::Captures,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        for expected in ["Captures (2)", "$.access_token", "Header", "Add capture"] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
    }

    #[test]
    fn tests_tab_lists_each_assertion() {
        let mut request = Request::new(Method::Get, "https://x.example/users/1")
            .with_assertion(Assertion::status(200))
            .with_assertion(Assertion::body("$.name", Operator::Contains, "Ada"));
        let mut state = EditorState {
            tab: Tab::Tests,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        for expected in ["Tests (2)", "$.name", "contains", "Ada", "Add test"] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
    }

    #[test]
    fn oauth2_fields_follow_the_grant() {
        let render = |grant: Grant| {
            let mut request = Request::new(Method::Get, "/").with_auth(Auth::OAuth2(OAuth2 {
                grant,
                ..Default::default()
            }));
            let mut state = EditorState {
                tab: Tab::Auth,
                ..Default::default()
            };
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 600.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
            });
            output.textures_delta.clear();
            format!("{:?}", output.shapes)
        };
        let credentials = render(Grant::ClientCredentials);
        for expected in [
            "Auth (OAuth 2.0)",
            "Token URL",
            "Client secret",
            "Basic auth header",
        ] {
            assert!(
                credentials.contains(expected),
                "{expected} in {credentials}"
            );
        }
        assert!(!credentials.contains("Redirect URI"));
        let code = render(Grant::AuthorizationCode);
        for expected in ["Authorization URL", "Redirect URI", "PKCE"] {
            assert!(code.contains(expected), "{expected} in {code}");
        }
    }

    #[test]
    fn grpc_requests_edit_a_json_message_instead_of_params() {
        let mut request = Request::new(Method::Post, "grpc://localhost/test.Greeter/SayHello");
        let mut state = EditorState {
            grpc: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        assert_eq!(state.tab, Tab::Body);
        assert_eq!(request.body, Body::None);
        assert!(
            text.contains("Message") && text.contains("client streaming"),
            "{text}"
        );
        assert!(
            !text.contains("Params") && !text.contains("Multipart"),
            "{text}"
        );
    }

    #[test]
    fn scripts_tab_edits_both_scripts_and_lists_the_helpers() {
        let mut request = Request::new(Method::Get, "https://api.example");
        request.scripts.post = "test(\"ok\", response.status == 200);".into();
        let mut state = EditorState {
            tab: Tab::Scripts,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        for expected in [
            "Scripts ●",
            "Pre-request",
            "Post-response",
            "response.status == 200",
            "Helpers",
        ] {
            assert!(text.contains(expected), "{expected} in {text}");
        }
    }

    #[test]
    fn websocket_requests_hide_body_captures_and_tests() {
        let mut request = Request::new(Method::Get, "ws://localhost/chat");
        let mut state = EditorState {
            tab: Tab::Tests,
            websocket: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_editor(ui, egui::Id::unique("editor"), &mut request, &mut state);
        });
        output.textures_delta.clear();
        let text = format!("{:?}", output.shapes);
        assert_eq!(state.tab, Tab::Params);
        assert!(text.contains("Headers") && text.contains("Auth"), "{text}");
        for hidden in ["Body", "Captures", "Tests"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
    }
}
