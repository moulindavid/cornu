use std::path::Path;

use cornu_core::{ApiKeyLocation, Auth, Body, KeyValue, Method, PartValue, Request};
use cornu_import::postman::parse_collection;
use cornu_import::{Collection, ImportError, ImportedFolder, ImportedItem};
use cornu_storage::{Item, Workspace};

const FIXTURE: &str = include_str!("fixtures/postman-v2.1.json");

fn collection() -> Collection {
    parse_collection(FIXTURE).unwrap()
}

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

#[test]
fn collection_settings_are_imported() {
    let root = collection().root;
    assert_eq!(root.name, "Pet Store");
    assert_eq!(
        root.auth,
        Auth::Bearer {
            token: "{{token}}".into()
        }
    );
    assert_eq!(
        root.variables[0],
        KeyValue::new("base_url", "https://petstore.example.com/v2")
    );
    assert_eq!(root.variables[1], KeyValue::new("retries", "3"));
    assert_eq!(
        root.variables[2],
        KeyValue::new("api_secret", "s3cret").secret()
    );
    assert_eq!(root.request_count(), 9);
}

#[test]
fn folders_keep_their_order_and_auth() {
    let root = collection().root;
    let names: Vec<_> = root
        .items
        .iter()
        .map(|item| match item {
            ImportedItem::Folder(folder) => folder.name.as_str(),
            ImportedItem::Request(request) => request.name.as_str(),
        })
        .collect();
    assert_eq!(
        names,
        [
            "Pets",
            "Countries (GraphQL)",
            "Upload raw file",
            "Plain XML",
            "Shorthand"
        ]
    );
    assert_eq!(
        folder(&root, "Pets").auth,
        Auth::ApiKey {
            key: "X-Api-Key".into(),
            value: "{{api_key}}".into(),
            location: ApiKeyLocation::Header,
        }
    );
}

#[test]
fn query_and_headers_keep_disabled_rows() {
    let root = collection().root;
    let find = request(folder(&root, "Pets"), "Find pets by status");
    assert_eq!(find.method, Method::Get);
    assert_eq!(find.url, "{{base_url}}/pet/findByStatus");
    assert_eq!(find.query.len(), 3);
    assert!(!find.query[2].enabled);
    assert_eq!(find.auth, Auth::Inherit);
    assert!(!find.headers[1].enabled);
    assert_eq!(find.headers[1].description.as_deref(), Some("Verbose logs"));
}

#[test]
fn path_variables_are_inlined() {
    let root = collection().root;
    assert_eq!(
        request(folder(&root, "Pets"), "Get pet").url,
        "{{base_url}}/pet/42"
    );
}

#[test]
fn bodies_are_converted() {
    let root = collection().root;
    let pets = folder(&root, "Pets");

    let add = request(pets, "Add pet");
    assert!(matches!(&add.body, Body::Json { text } if text.contains("{{pet_name}}")));

    let upload = request(pets, "Upload image");
    assert_eq!(upload.auth, Auth::None);
    let Body::Multipart { parts } = &upload.body else {
        panic!("expected multipart");
    };
    assert_eq!(parts[1].value, PartValue::File("/tmp/dog.png".into()));
    assert_eq!(parts[1].content_type.as_deref(), Some("image/png"));
    assert!(!parts[2].enabled);

    let login = request(folder(pets, "Nested"), "Login");
    assert_eq!(login.url, "https://auth.example.com/oauth/token");
    assert!(matches!(&login.auth, Auth::Basic { username, .. } if username == "admin"));
    assert!(matches!(&login.body, Body::FormUrlEncoded { fields } if !fields[1].enabled));

    let graphql = request(&root, "Countries (GraphQL)");
    let Body::GraphQl { query, variables } = &graphql.body else {
        panic!("expected graphql");
    };
    assert!(query.contains("country"));
    let variables: serde_json::Value = serde_json::from_str(variables).unwrap();
    assert_eq!(variables["code"], "FR");

    let file = request(&root, "Upload raw file");
    assert!(matches!(&file.body, Body::Binary { path, .. } if path == Path::new("/tmp/data.bin")));

    let xml = request(&root, "Plain XML");
    assert_eq!(xml.method, Method::Post);
    assert_eq!(xml.headers[1], KeyValue::new("X-Trace", "on"));
    assert!(
        matches!(&xml.body, Body::Raw { content_type, .. } if content_type == "application/xml")
    );

    let shorthand = request(&root, "Shorthand");
    assert_eq!(shorthand.url, "https://example.com/ping");
    assert_eq!(shorthand.query, [KeyValue::new("verbose", "1")]);
}

#[test]
fn unsupported_parts_produce_warnings() {
    let warnings = collection().warnings;
    let all = warnings.join("\n");
    assert!(all.contains("oauth2 auth is not supported"), "{all}");
    assert!(
        all.contains("2 pre-request or test script(s) skipped"),
        "{all}"
    );
    assert!(all.contains("1 saved example response(s) skipped"), "{all}");
    assert_eq!(warnings.len(), 3, "{all}");
}

#[test]
fn v2_0_auth_objects_and_api_wrapper_are_understood() {
    let json = r#"{"collection": {
        "info": {"name": "Old", "schema": "https://schema.getpostman.com/json/collection/v2.0.0/collection.json"},
        "auth": {"type": "bearer", "bearer": {"token": "abc"}},
        "item": []
    }}"#;
    let root = parse_collection(json).unwrap().root;
    assert_eq!(root.name, "Old");
    assert_eq!(
        root.auth,
        Auth::Bearer {
            token: "abc".into()
        }
    );
}

#[test]
fn other_formats_are_rejected_with_a_clear_message() {
    let v1 = r#"{"id": "x", "name": "Old", "requests": []}"#;
    assert!(matches!(
        parse_collection(v1),
        Err(ImportError::Unsupported(message)) if message.contains("v1")
    ));
    assert!(matches!(
        parse_collection("[]"),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(parse_collection("{"), Err(ImportError::Json(_))));
}

#[test]
fn imported_collection_is_written_as_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::init(dir.path(), "Demo").unwrap();
    let collection = collection();
    let path = collection
        .root
        .write_into(&mut workspace, Path::new(""))
        .unwrap();
    assert_eq!(path, Path::new("pet-store"));

    let loaded = Workspace::load(dir.path()).unwrap();
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.tree, workspace.tree);

    let Item::Folder(store) = &loaded.tree.items[0] else {
        panic!("expected the collection folder");
    };
    assert_eq!(store.settings.auth, collection.root.auth);
    let shared = std::fs::read_to_string(dir.path().join("pet-store/_folder.toml")).unwrap();
    assert!(!shared.contains("s3cret"), "{shared}");
    assert_eq!(store.items.len(), 5);
    let login = loaded
        .find_request(Path::new("pet-store/pets/nested/login.req.toml"))
        .unwrap();
    assert_eq!(login.request.url, "https://auth.example.com/oauth/token");
}
