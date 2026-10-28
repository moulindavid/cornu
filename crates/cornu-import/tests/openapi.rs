use cornu_core::{ApiKeyLocation, Auth, Body, KeyValue, Method, MultipartPart, Request};
use cornu_import::openapi::parse_spec;
use cornu_import::{Collection, ImportError, ImportedFolder, ImportedItem};
use cornu_storage::Workspace;
use serde_json::{Value, json};

const OPENAPI_3: &str = include_str!("fixtures/openapi-3.yaml");
const SWAGGER_2: &str = include_str!("fixtures/swagger-2.json");

fn folder<'a>(parent: &'a ImportedFolder, name: &str) -> &'a ImportedFolder {
    parent
        .items
        .iter()
        .find_map(|item| match item {
            ImportedItem::Folder(folder) if folder.name == name => Some(folder),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no folder {name}"))
}

fn request<'a>(parent: &'a ImportedFolder, name: &str) -> &'a Request {
    parent
        .items
        .iter()
        .find_map(|item| match item {
            ImportedItem::Request(request) if request.name == name => Some(request),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no request {name}"))
}

fn json_body(request: &Request) -> Value {
    match &request.body {
        Body::Json { text } => serde_json::from_str(text).unwrap(),
        other => panic!("not a JSON body: {other:?}"),
    }
}

fn names(folder: &ImportedFolder) -> Vec<&str> {
    folder
        .items
        .iter()
        .map(|item| match item {
            ImportedItem::Folder(folder) => folder.name.as_str(),
            ImportedItem::Request(request) => request.name.as_str(),
        })
        .collect()
}

fn openapi() -> Collection {
    parse_spec(OPENAPI_3, None).unwrap()
}

#[test]
fn openapi_3_tags_become_folders_and_servers_the_base_url() {
    let collection = openapi();
    let root = &collection.root;
    assert_eq!(root.name, "Swagger Petstore - OpenAPI 3.0");
    assert_eq!(names(root), ["pet", "store", "Health check"]);
    assert_eq!(
        names(folder(root, "pet")),
        [
            "Add a new pet to the store",
            "Finds Pets by status",
            "getPetById",
            "Deletes a pet",
            "uploads an image",
        ]
    );
    let mut token = KeyValue::new("token", "");
    token.secret = true;
    let mut api_key = KeyValue::new("api_key", "");
    api_key.secret = true;
    assert_eq!(
        root.variables,
        [
            KeyValue::new("base_url", "https://eu.petstore.example/api/v3"),
            token,
            api_key
        ]
    );
    assert_eq!(collection.root.request_count(), 7);
}

#[test]
fn openapi_3_parameters_and_auth() {
    let collection = openapi();
    let pet = folder(&collection.root, "pet");

    let find = request(pet, "Finds Pets by status");
    assert_eq!(find.method, Method::Get);
    assert_eq!(find.url, "{{base_url}}/pet/findByStatus");
    let mut limit = KeyValue::new("limit", "");
    limit.enabled = false;
    assert_eq!(find.query, [KeyValue::new("status", "available"), limit]);
    assert!(collection.warnings.iter().any(|w| w.contains("1 cookie")));

    let get = request(pet, "getPetById");
    assert_eq!(get.url, "{{base_url}}/pet/{{petId}}");
    let mut request_id = KeyValue::new("X-Request-Id", "req-1");
    request_id.enabled = false;
    assert_eq!(get.headers, [request_id]);
    assert_eq!(get.auth, Auth::Inherit);

    assert_eq!(
        collection.root.auth,
        Auth::Bearer {
            token: "{{token}}".into()
        }
    );
    assert_eq!(
        request(pet, "Deletes a pet").auth,
        Auth::ApiKey {
            key: "api_key".into(),
            value: "{{api_key}}".into(),
            location: ApiKeyLocation::Header,
        }
    );
    assert_eq!(request(&collection.root, "Health check").auth, Auth::None);
}

#[test]
fn openapi_3_bodies_are_generated_from_schemas() {
    let collection = openapi();
    let pet = folder(&collection.root, "pet");

    let add = request(pet, "Add a new pet to the store");
    assert_eq!(add.method, Method::Post);
    assert!(add.headers.is_empty());
    let body = json_body(add);
    assert_eq!(body["id"], json!(10));
    assert_eq!(body["name"], json!("doggie"));
    assert_eq!(body["category"], json!({"id": 1, "name": "Dogs"}));
    assert_eq!(body["photoUrls"], json!(["string"]));
    assert_eq!(body["status"], json!("available"));
    assert_eq!(body["parent"], Value::Null, "recursive schemas stop");

    assert_eq!(
        request(pet, "uploads an image").body,
        Body::Multipart {
            parts: vec![
                MultipartPart::text("additionalMetadata", "string"),
                MultipartPart::file("file", ""),
            ]
        }
    );
    let order = request(
        folder(&collection.root, "store"),
        "Place an order for a pet",
    );
    assert_eq!(
        order.body,
        Body::FormUrlEncoded {
            fields: vec![
                KeyValue::new("petId", "198772"),
                KeyValue::new("quantity", "7"),
                KeyValue::new("shipDate", "2024-01-01T00:00:00Z"),
                KeyValue::new("complete", "true"),
            ]
        }
    );
}

#[test]
fn swagger_2_is_imported() {
    let collection = parse_spec(SWAGGER_2, None).unwrap();
    let root = &collection.root;
    assert_eq!(root.name, "Swagger Petstore");
    assert_eq!(
        root.variables[0],
        KeyValue::new("base_url", "https://petstore.swagger.io/v2")
    );
    assert_eq!(
        root.auth,
        Auth::Basic {
            username: "{{username}}".into(),
            password: "{{password}}".into()
        }
    );

    let pet = folder(root, "pet");
    let update = request(pet, "Update an existing pet");
    assert_eq!(update.method, Method::Put);
    assert_eq!(
        json_body(update),
        json!({"id": 0, "name": "doggie", "tags": [{"id": 0, "name": "string"}]})
    );
    assert_eq!(
        update.auth,
        Auth::Bearer {
            token: "{{token}}".into()
        }
    );
    assert!(collection.warnings.iter().any(|w| w.contains("oauth2")));

    assert_eq!(
        request(pet, "Updates a pet in the store with form data").body,
        Body::FormUrlEncoded {
            fields: vec![KeyValue::new("name", "rex"), KeyValue::new("status", "")]
        }
    );
    assert_eq!(
        request(pet, "uploads an image").body,
        Body::Multipart {
            parts: vec![
                MultipartPart::text("additionalMetadata", ""),
                MultipartPart::file("file", ""),
            ]
        }
    );

    let login = request(folder(root, "user"), "Logs user into the system");
    assert_eq!(login.auth, Auth::None);
    assert_eq!(login.query[0], KeyValue::new("username", ""));
    assert_eq!(login.query[1].value, "a");
}

#[test]
fn relative_servers_use_the_download_url() {
    let spec = r#"{"openapi": "3.1.0", "info": {"title": "T"}, "servers": [{"url": "/api"}],
        "paths": {"/a": {"get": {}}}}"#;
    let collection = parse_spec(spec, Some("https://host.example/docs/openapi.json")).unwrap();
    assert_eq!(
        collection.root.variables[0],
        KeyValue::new("base_url", "https://host.example/api")
    );
    assert_eq!(names(&collection.root), ["GET /a"]);
    assert_eq!(collection.root.auth, Auth::None);

    let collection = parse_spec(spec, None).unwrap();
    assert_eq!(collection.root.variables[0].value, "/api");
    assert!(collection.warnings.iter().any(|w| w.contains("base_url")));
}

#[test]
fn json_variants_and_openapi_3_1_types() {
    let spec = json!({
        "openapi": "3.1.0",
        "info": {"title": "T"},
        "servers": [{"url": "https://x.example/"}],
        "paths": {"/a": {"post": {"requestBody": {"content": {
            "application/vnd.api+json": {"schema": {"type": "object", "properties": {
                "note": {"type": ["string", "null"]},
                "kind": {"const": "a"},
                "shape": {"oneOf": [{"type": "integer"}, {"type": "string"}]}
            }}}
        }}}}}
    });
    let collection = parse_spec(&spec.to_string(), None).unwrap();
    let request = request(&collection.root, "POST /a");
    assert_eq!(collection.root.variables[0].value, "https://x.example");
    assert_eq!(
        request.headers,
        [KeyValue::new("Content-Type", "application/vnd.api+json")]
    );
    assert_eq!(
        json_body(request),
        json!({"note": "string", "kind": "a", "shape": 0})
    );
}

#[test]
fn unsupported_documents_are_rejected() {
    for text in [
        r#"{"openapi": "4.0.0"}"#,
        r#"{"swagger": "1.2"}"#,
        "info:\n  title: nope\n",
        "[1, 2]",
    ] {
        assert!(
            matches!(parse_spec(text, None), Err(ImportError::Unsupported(_))),
            "{text}"
        );
    }
    assert!(matches!(
        parse_spec("{oops", None),
        Err(ImportError::Json(_))
    ));
    assert!(matches!(
        parse_spec("a: [unclosed", None),
        Err(ImportError::Unsupported(message)) if message.contains("YAML")
    ));
}

#[test]
fn imported_spec_is_written_to_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::init(dir.path(), "Demo").unwrap();
    let collection = openapi();
    let path = collection
        .root
        .write_into(&mut workspace, std::path::Path::new(""))
        .unwrap();

    let loaded = Workspace::load(dir.path()).unwrap();
    let folder = loaded.folder(&path).unwrap();
    assert_eq!(folder.settings.variables, collection.root.variables);
    let saved = loaded
        .find_request(&path.join("pet/add-a-new-pet-to-the-store.req.toml"))
        .unwrap();
    assert_eq!(saved.request.name, "Add a new pet to the store");
}
