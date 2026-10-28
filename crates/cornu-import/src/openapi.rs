//! OpenAPI 3.0 / 3.1 and Swagger 2.0 specs, in JSON or YAML.

use cornu_core::{ApiKeyLocation, Auth, Body, KeyValue, Method, MultipartPart, Request};
use serde_json::{Map, Value, json};

use crate::{Collection, ImportError, ImportedFolder, ImportedItem};

const METHODS: [(&str, Method); 8] = [
    ("get", Method::Get),
    ("put", Method::Put),
    ("post", Method::Post),
    ("delete", Method::Delete),
    ("options", Method::Options),
    ("head", Method::Head),
    ("patch", Method::Patch),
    ("trace", Method::Trace),
];

/// Schemas nest deeper than any useful example; past this, values are left out.
const MAX_DEPTH: usize = 8;

/// `source` is where the spec was downloaded from, used to complete relative server URLs.
pub fn parse_spec(text: &str, source: Option<&str>) -> Result<Collection, ImportError> {
    let spec = parse_document(text)?;
    let Some(root) = spec.as_object() else {
        return Err(ImportError::Unsupported("not an OpenAPI document".into()));
    };
    let version = if let Some(version) = root.get("openapi").and_then(Value::as_str) {
        if !version.starts_with("3.") {
            return Err(ImportError::Unsupported(format!(
                "OpenAPI {version} is not supported"
            )));
        }
        Version::V3
    } else if root.get("swagger").and_then(Value::as_str) == Some("2.0") {
        Version::V2
    } else {
        return Err(ImportError::Unsupported(
            "not an OpenAPI 3 or Swagger 2 document".into(),
        ));
    };

    let mut importer = Importer {
        spec: &spec,
        version,
        warnings: Vec::new(),
        variables: Vec::new(),
        cookie_params: 0,
        expanding: Vec::new(),
    };
    let base_url = importer.base_url(source);
    importer.variables.push(KeyValue::new("base_url", base_url));
    let root_auth = importer
        .security(root.get("security"))
        .unwrap_or(Auth::None);

    let mut folders: Vec<ImportedFolder> = root
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| text_of(tag.get("name")))
        .map(|name| ImportedFolder {
            name,
            ..Default::default()
        })
        .collect();
    let mut untagged = Vec::new();
    for (path, item) in root
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let Some(item) = importer.resolve(item).and_then(Value::as_object) else {
            continue;
        };
        for (key, method) in METHODS {
            let Some(operation) = item.get(key).and_then(Value::as_object) else {
                continue;
            };
            let request = importer.request(path, method, item, operation, &root_auth);
            let tag = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| text_of(tags.first()));
            match tag {
                Some(tag) => {
                    let index = match folders.iter().position(|folder| folder.name == tag) {
                        Some(index) => index,
                        None => {
                            folders.push(ImportedFolder {
                                name: tag,
                                ..Default::default()
                            });
                            folders.len() - 1
                        }
                    };
                    folders[index].items.push(ImportedItem::Request(request));
                }
                None => untagged.push(ImportedItem::Request(request)),
            }
        }
    }

    let mut items: Vec<ImportedItem> = folders
        .into_iter()
        .filter(|folder| !folder.items.is_empty())
        .map(ImportedItem::Folder)
        .collect();
    items.extend(untagged);

    let info = root.get("info");
    let name = text_of(info.and_then(|info| info.get("title")))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Imported API".into());
    let variables = std::mem::take(&mut importer.variables);
    Ok(Collection {
        root: ImportedFolder {
            name,
            variables,
            auth: root_auth,
            items,
        },
        warnings: importer.finish(),
    })
}

fn parse_document(text: &str) -> Result<Value, ImportError> {
    if text.trim_start().starts_with('{') {
        return Ok(serde_json::from_str(text)?);
    }
    let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|error| ImportError::Unsupported(format!("invalid YAML: {error}")))?;
    Ok(yaml_to_json(yaml))
}

/// YAML allows non-string keys such as `200:` for responses, which JSON objects do not.
fn yaml_to_json(value: serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Yaml;
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(flag) => Value::Bool(flag),
        Yaml::Number(number) => number
            .as_i64()
            .map(Value::from)
            .or_else(|| number.as_u64().map(Value::from))
            .or_else(|| number.as_f64().map(Value::from))
            .unwrap_or(Value::Null),
        Yaml::String(text) => Value::String(text),
        Yaml::Sequence(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Mapping(mapping) => Value::Object(
            mapping
                .into_iter()
                .map(|(key, value)| {
                    let key = match yaml_to_json(key) {
                        Value::String(key) => key,
                        other => other.to_string(),
                    };
                    (key, yaml_to_json(value))
                })
                .collect(),
        ),
        Yaml::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V2,
    V3,
}

struct Importer<'a> {
    spec: &'a Value,
    version: Version,
    warnings: Vec<String>,
    variables: Vec<KeyValue>,
    cookie_params: usize,
    /// Schemas being expanded, so self-referencing ones stop instead of nesting to `MAX_DEPTH`.
    expanding: Vec<&'a str>,
}

impl<'a> Importer<'a> {
    fn finish(mut self) -> Vec<String> {
        if self.cookie_params > 0 {
            self.warnings.push(format!(
                "{} cookie parameter(s) skipped",
                self.cookie_params
            ));
        }
        self.warnings
    }

    /// Follows local `$ref`s; external ones are reported and left unresolved.
    fn resolve(&mut self, value: &'a Value) -> Option<&'a Value> {
        let mut value = value;
        for _ in 0..MAX_DEPTH {
            let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
                return Some(value);
            };
            let Some(pointer) = reference.strip_prefix('#') else {
                let warning = format!("external reference {reference} not followed");
                if !self.warnings.contains(&warning) {
                    self.warnings.push(warning);
                }
                return None;
            };
            value = self.spec.pointer(pointer)?;
        }
        None
    }

    fn base_url(&mut self, source: Option<&str>) -> String {
        let root = self.spec;
        let source = source.and_then(|source| url::Url::parse(source).ok());
        let url = match self.version {
            Version::V3 => {
                let server = root
                    .get("servers")
                    .and_then(Value::as_array)
                    .and_then(|servers| servers.first());
                let mut url = text_of(server.and_then(|server| server.get("url")))
                    .unwrap_or_else(|| "/".into());
                for (name, variable) in server
                    .and_then(|server| server.get("variables"))
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    let default = text_of(variable.get("default")).unwrap_or_default();
                    url = url.replace(&format!("{{{name}}}"), &default);
                }
                url
            }
            Version::V2 => {
                let base_path = text_of(root.get("basePath")).unwrap_or_default();
                match text_of(root.get("host")) {
                    Some(host) => {
                        let scheme = root
                            .get("schemes")
                            .and_then(Value::as_array)
                            .and_then(|schemes| text_of(schemes.first()))
                            .or_else(|| source.as_ref().map(|source| source.scheme().to_owned()))
                            .unwrap_or_else(|| "https".into());
                        format!("{scheme}://{host}{base_path}")
                    }
                    None => base_path,
                }
            }
        };
        if url.contains("://") {
            return url.trim_end_matches('/').to_owned();
        }
        match source.and_then(|source| source.join(&url).ok()) {
            Some(joined) => joined.as_str().trim_end_matches('/').to_owned(),
            None => {
                self.warnings.push(format!(
                    "the spec has no absolute server URL: set base_url (now {url:?})"
                ));
                url.trim_end_matches('/').to_owned()
            }
        }
    }

    fn request(
        &mut self,
        path: &str,
        method: Method,
        item: &'a Map<String, Value>,
        operation: &'a Map<String, Value>,
        root_auth: &Auth,
    ) -> Request {
        let name = text_of(operation.get("summary"))
            .or_else(|| text_of(operation.get("operationId")))
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("{method} {path}"));
        let mut request = Request::new(method, format!("{{{{base_url}}}}{}", template(path)))
            .with_name(name.trim());

        let mut form = Vec::new();
        let mut body_schema = None;
        for parameter in self.parameters(item, operation) {
            let name = text_of(parameter.get("name")).unwrap_or_default();
            match parameter.get("in").and_then(Value::as_str) {
                Some("query") => request.query.push(self.parameter_row(name, parameter)),
                Some("header") if !is_reserved_header(&name) => {
                    request.headers.push(self.parameter_row(name, parameter));
                }
                Some("cookie") => self.cookie_params += 1,
                Some("formData") => form.push(parameter),
                Some("body") => body_schema = parameter.get("schema"),
                _ => {}
            }
        }

        let (body, content_type) = match self.version {
            Version::V3 => self.body_v3(operation.get("requestBody")),
            Version::V2 => self.body_v2(operation, body_schema, &form),
        };
        if let Some(content_type) = content_type {
            request
                .headers
                .push(KeyValue::new("Content-Type", content_type));
        }
        request.body = body;

        if let Some(auth) = self.security(operation.get("security"))
            && &auth != root_auth
        {
            request.auth = auth;
        }
        request
    }

    /// Path-level parameters apply to every operation unless the operation redefines them.
    fn parameters(
        &mut self,
        item: &'a Map<String, Value>,
        operation: &'a Map<String, Value>,
    ) -> Vec<&'a Value> {
        let mut parameters: Vec<&'a Value> = Vec::new();
        for list in [item.get("parameters"), operation.get("parameters")] {
            for parameter in list.and_then(Value::as_array).into_iter().flatten() {
                let Some(parameter) = self.resolve(parameter) else {
                    continue;
                };
                let key = |parameter: &Value| {
                    (text_of(parameter.get("name")), text_of(parameter.get("in")))
                };
                parameters.retain(|existing| key(existing) != key(parameter));
                parameters.push(parameter);
            }
        }
        parameters
    }

    fn parameter_row(&mut self, name: String, parameter: &'a Value) -> KeyValue {
        let example = parameter
            .get("example")
            .or_else(|| parameter.get("x-example"))
            .cloned()
            .or_else(|| first_example(parameter.get("examples")))
            .or_else(|| {
                let schema = parameter.get("schema").unwrap_or(parameter);
                self.scalar_example(schema)
            });
        let mut row = KeyValue::new(name, example.map(|value| plain(&value)).unwrap_or_default());
        row.enabled = parameter.get("required").and_then(Value::as_bool) == Some(true);
        row.description = text_of(parameter.get("description")).filter(|text| !text.is_empty());
        row
    }

    /// Only explicit values: a generated `"string"` placeholder in a query row is noise.
    fn scalar_example(&mut self, schema: &'a Value) -> Option<Value> {
        let schema = self.resolve(schema)?;
        schema
            .get("example")
            .or_else(|| schema.get("x-example"))
            .or_else(|| schema.get("default"))
            .or_else(|| schema.get("enum").and_then(|values| values.get(0)))
            .cloned()
            .or_else(|| {
                schema
                    .get("items")
                    .and_then(|items| self.scalar_example(items))
            })
    }

    fn body_v3(&mut self, request_body: Option<&'a Value>) -> (Body, Option<String>) {
        let Some(content) = request_body
            .and_then(|body| self.resolve(body))
            .and_then(|body| body.get("content"))
            .and_then(Value::as_object)
        else {
            return (Body::None, None);
        };
        let preference = |media: &str| {
            let media = media.to_ascii_lowercase();
            if media.contains("json") {
                0
            } else if media == "application/x-www-form-urlencoded" {
                1
            } else if media == "multipart/form-data" {
                2
            } else {
                3
            }
        };
        let Some((media, entry)) = content.iter().min_by_key(|(media, _)| preference(media)) else {
            return (Body::None, None);
        };
        let schema = entry.get("schema");
        let explicit = entry
            .get("example")
            .cloned()
            .or_else(|| first_example(entry.get("examples")));
        let media_lower = media.to_ascii_lowercase();

        if media_lower.contains("json") {
            let example = explicit
                .or_else(|| schema.map(|schema| self.example(schema, 0)))
                .unwrap_or_else(|| json!({}));
            let text = serde_json::to_string_pretty(&example).unwrap_or_default();
            let header = (media_lower != "application/json").then(|| media.clone());
            return (Body::Json { text }, header);
        }
        let properties = schema
            .map(|schema| self.properties(schema))
            .unwrap_or_default();
        match media_lower.as_str() {
            "application/x-www-form-urlencoded" => {
                let fields = properties
                    .into_iter()
                    .map(|(name, schema)| {
                        let value = self.example(schema, 1);
                        KeyValue::new(name, plain(&value))
                    })
                    .collect();
                (Body::FormUrlEncoded { fields }, None)
            }
            "multipart/form-data" => {
                let parts = properties
                    .into_iter()
                    .map(|(name, schema)| {
                        if self.is_binary(schema) {
                            MultipartPart::file(name, "")
                        } else {
                            let value = self.example(schema, 1);
                            MultipartPart::text(name, plain(&value))
                        }
                    })
                    .collect();
                (Body::Multipart { parts }, None)
            }
            _ if schema.is_some_and(|schema| self.is_binary(schema))
                || media_lower == "application/octet-stream"
                || media_lower.starts_with("image/") =>
            {
                let body = Body::Binary {
                    path: Default::default(),
                    content_type: Some(media.clone()),
                };
                (body, None)
            }
            _ => {
                let text = explicit.map(|value| plain(&value)).unwrap_or_default();
                let body = Body::Raw {
                    text,
                    content_type: media.clone(),
                };
                (body, None)
            }
        }
    }

    fn body_v2(
        &mut self,
        operation: &'a Map<String, Value>,
        schema: Option<&'a Value>,
        form: &[&'a Value],
    ) -> (Body, Option<String>) {
        let consumes: Vec<String> = operation
            .get("consumes")
            .or_else(|| self.spec.get("consumes"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|media| text_of(Some(media)))
            .collect();
        if !form.is_empty() {
            let multipart = consumes.iter().any(|media| media == "multipart/form-data")
                || form
                    .iter()
                    .any(|parameter| parameter.get("type").and_then(Value::as_str) == Some("file"));
            if multipart {
                let parts = form
                    .iter()
                    .map(|parameter| {
                        let name = text_of(parameter.get("name")).unwrap_or_default();
                        if parameter.get("type").and_then(Value::as_str) == Some("file") {
                            MultipartPart::file(name, "")
                        } else {
                            let value = self.scalar_example(parameter).unwrap_or(Value::Null);
                            MultipartPart::text(name, plain(&value))
                        }
                    })
                    .collect();
                return (Body::Multipart { parts }, None);
            }
            let fields = form
                .iter()
                .map(|parameter| {
                    let name = text_of(parameter.get("name")).unwrap_or_default();
                    let value = self.scalar_example(parameter).unwrap_or(Value::Null);
                    KeyValue::new(name, plain(&value))
                })
                .collect();
            return (Body::FormUrlEncoded { fields }, None);
        }
        let Some(schema) = schema else {
            return (Body::None, None);
        };
        let media = consumes
            .iter()
            .find(|media| media.contains("json"))
            .or_else(|| consumes.first())
            .cloned()
            .unwrap_or_else(|| "application/json".into());
        if media.contains("json") {
            let example = self.example(schema, 0);
            let text = serde_json::to_string_pretty(&example).unwrap_or_default();
            let header = (media != "application/json").then_some(media);
            return (Body::Json { text }, header);
        }
        let body = Body::Raw {
            text: String::new(),
            content_type: media,
        };
        (body, None)
    }

    fn properties(&mut self, schema: &'a Value) -> Vec<(String, &'a Value)> {
        let Some(schema) = self.resolve(schema) else {
            return Vec::new();
        };
        let mut properties = Vec::new();
        for part in schema
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            properties.extend(self.properties(part));
        }
        for (name, property) in schema
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            properties.push((name.clone(), property));
        }
        properties
    }

    fn is_binary(&mut self, schema: &'a Value) -> bool {
        let Some(schema) = self.resolve(schema) else {
            return false;
        };
        if let Some(items) = schema.get("items") {
            return self.is_binary(items);
        }
        matches!(
            schema.get("format").and_then(Value::as_str),
            Some("binary" | "base64")
        ) || schema.get("contentMediaType").is_some()
    }

    fn example(&mut self, schema: &'a Value, depth: usize) -> Value {
        if depth > MAX_DEPTH {
            return Value::Null;
        }
        let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
            return self.inline_example(schema, depth);
        };
        if self.expanding.contains(&reference) {
            return Value::Null;
        }
        self.expanding.push(reference);
        let example = self.inline_example(schema, depth);
        self.expanding.pop();
        example
    }

    fn inline_example(&mut self, schema: &'a Value, depth: usize) -> Value {
        let Some(schema) = self.resolve(schema) else {
            return Value::Null;
        };
        if let Some(example) = schema
            .get("example")
            .cloned()
            .or_else(|| {
                schema
                    .get("examples")
                    .and_then(|examples| examples.get(0))
                    .cloned()
            })
            .or_else(|| schema.get("default").cloned())
            .or_else(|| schema.get("const").cloned())
            .or_else(|| schema.get("enum").and_then(|values| values.get(0)).cloned())
        {
            return example;
        }
        if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
            let mut merged = Map::new();
            for part in parts {
                match self.example(part, depth + 1) {
                    Value::Object(object) => merged.extend(object),
                    other if parts.len() == 1 => return other,
                    _ => {}
                }
            }
            return Value::Object(merged);
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(first) = schema.get(key).and_then(|options| options.get(0)) {
                return self.example(first, depth + 1);
            }
        }

        match schema_type(schema) {
            Some("object") => {
                let object = self
                    .properties(schema)
                    .into_iter()
                    .map(|(name, property)| (name, self.example(property, depth + 1)))
                    .collect();
                Value::Object(object)
            }
            Some("array") => match schema.get("items") {
                Some(items) => json!([self.example(items, depth + 1)]),
                None => json!([]),
            },
            Some("integer") | Some("number") => json!(0),
            Some("boolean") => json!(true),
            Some("null") => Value::Null,
            Some(_) => {
                let text = match schema.get("format").and_then(Value::as_str) {
                    Some("date-time") => "2024-01-01T00:00:00Z",
                    Some("date") => "2024-01-01",
                    Some("email") => "user@example.com",
                    Some("uuid") => "00000000-0000-0000-0000-000000000000",
                    Some("uri" | "url") => "https://example.com",
                    _ => "string",
                };
                json!(text)
            }
            None => Value::Null,
        }
    }

    /// `None` when `requirements` is absent, so the caller can fall back to the parent.
    fn security(&mut self, requirements: Option<&'a Value>) -> Option<Auth> {
        let requirements = requirements?.as_array()?;
        // `[{}]` or `[]` both mean no auth is required.
        let Some((name, _)) = requirements
            .iter()
            .filter_map(Value::as_object)
            .find_map(|requirement| requirement.iter().next())
        else {
            return Some(Auth::None);
        };
        let schemes = match self.version {
            Version::V3 => self.spec.pointer("/components/securitySchemes"),
            Version::V2 => self.spec.get("securityDefinitions"),
        };
        let Some(scheme) = schemes
            .and_then(|schemes| schemes.get(name))
            .and_then(|scheme| self.resolve(scheme))
        else {
            self.warn(format!("security scheme {name:?} is not defined"));
            return Some(Auth::None);
        };
        Some(self.scheme_auth(name, scheme))
    }

    fn scheme_auth(&mut self, name: &str, scheme: &Value) -> Auth {
        let kind = scheme
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let http_scheme = scheme
            .get("scheme")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase);
        match (kind, http_scheme.as_deref()) {
            ("http", Some("bearer")) => self.bearer(),
            ("http", Some("basic")) | ("basic", _) => {
                self.variable("username", false);
                self.variable("password", true);
                Auth::Basic {
                    username: "{{username}}".into(),
                    password: "{{password}}".into(),
                }
            }
            ("apiKey", _) => {
                let key = text_of(scheme.get("name")).unwrap_or_default();
                let location = match scheme.get("in").and_then(Value::as_str) {
                    Some("header") => ApiKeyLocation::Header,
                    Some("query") => ApiKeyLocation::Query,
                    _ => {
                        self.warn(format!(
                            "API key {name:?} is sent as a cookie: not supported"
                        ));
                        return Auth::None;
                    }
                };
                self.variable("api_key", true);
                Auth::ApiKey {
                    key,
                    value: "{{api_key}}".into(),
                    location,
                }
            }
            ("oauth2" | "openIdConnect", _) => {
                self.warn(format!(
                    "{name:?} uses {kind}: cornu cannot run the flow yet, paste an access token into the token variable"
                ));
                self.bearer()
            }
            _ => {
                self.warn(format!(
                    "security scheme {name:?} ({kind}) is not supported"
                ));
                Auth::None
            }
        }
    }

    fn bearer(&mut self) -> Auth {
        self.variable("token", true);
        Auth::Bearer {
            token: "{{token}}".into(),
        }
    }

    fn variable(&mut self, key: &str, secret: bool) {
        if self.variables.iter().all(|variable| variable.key != key) {
            let mut variable = KeyValue::new(key, "");
            variable.secret = secret;
            self.variables.push(variable);
        }
    }

    fn warn(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }
}

/// OpenAPI 3.1 allows `type: [string, "null"]`.
fn schema_type(schema: &Value) -> Option<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => Some(kind),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null"),
        _ if schema.get("properties").is_some() => Some("object"),
        _ => None,
    }
}

fn first_example(examples: Option<&Value>) -> Option<Value> {
    match examples? {
        Value::Object(map) => map.values().next()?.get("value").cloned(),
        Value::Array(list) => list.first().cloned(),
        _ => None,
    }
}

/// OpenAPI says these header parameters are ignored: they come from the body and auth.
fn is_reserved_header(name: &str) -> bool {
    ["accept", "content-type", "authorization"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// `/pets/{petId}` becomes `/pets/{{petId}}`.
fn template(path: &str) -> String {
    let mut result = String::with_capacity(path.len() + 8);
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        result.push_str(&rest[..start]);
        result.push_str("{{");
        result.push_str(&rest[start + 1..start + end]);
        result.push_str("}}");
        rest = &rest[start + end + 1..];
    }
    result.push_str(rest);
    result
}

fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_templates_become_variables() {
        assert_eq!(
            template("/pets/{petId}/toys/{id}"),
            "/pets/{{petId}}/toys/{{id}}"
        );
        assert_eq!(template("/plain"), "/plain");
        assert_eq!(template("/broken/{x"), "/broken/{x");
    }

    #[test]
    fn yaml_numeric_keys_become_strings() {
        let value = parse_document("responses:\n  200:\n    description: ok\n").unwrap();
        assert_eq!(
            value.pointer("/responses/200/description"),
            Some(&json!("ok"))
        );
    }
}
