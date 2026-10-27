use cornu_core::{Environment, KeyValue};
use cornu_import::ImportError;
use cornu_import::postman::parse_environment;
use cornu_storage::Workspace;

const FIXTURE: &str = include_str!("fixtures/postman-environment.json");

#[test]
fn environment_variables_are_imported() {
    let environment = parse_environment(FIXTURE).unwrap();
    assert_eq!(environment.name, "Pet Store Dev");

    let mut token = KeyValue::new("token", "dev-s3cret");
    token.secret = true;
    let mut legacy = KeyValue::new("legacy_host", "old.petstore.example");
    legacy.enabled = false;
    assert_eq!(
        environment.variables,
        vec![
            KeyValue::new("base_url", "https://dev.petstore.example/v2"),
            token,
            KeyValue::new("retries", "3"),
            legacy,
        ]
    );
}

#[test]
fn api_wrapper_and_globals_exports_are_accepted() {
    let wrapped = r#"{"environment": {"name": "Prod", "values": [{"key": "a", "value": "1"}]}}"#;
    assert_eq!(
        parse_environment(wrapped).unwrap(),
        Environment::new("Prod").with_variable("a", "1")
    );

    let globals = r#"{"name": "Globals", "_postman_variable_scope": "globals", "values": []}"#;
    assert_eq!(
        parse_environment(globals).unwrap(),
        Environment::new("Globals")
    );
}

#[test]
fn collections_and_garbage_are_rejected() {
    let collection = include_str!("fixtures/postman-v2.1.json");
    assert!(matches!(
        parse_environment(collection),
        Err(ImportError::Unsupported(message)) if message.contains("collection")
    ));
    assert!(matches!(
        parse_environment(r#"{"name": "x"}"#),
        Err(ImportError::Unsupported(_))
    ));
    assert!(matches!(parse_environment("[1"), Err(ImportError::Json(_))));
}

#[test]
fn imported_secrets_stay_out_of_versioned_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::init(dir.path(), "Demo").unwrap();
    workspace
        .create_environment(parse_environment(FIXTURE).unwrap())
        .unwrap();

    let versioned =
        std::fs::read_to_string(dir.path().join("environments/pet-store-dev.toml")).unwrap();
    assert!(versioned.contains("dev.petstore.example"));
    assert!(!versioned.contains("dev-s3cret"));

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(
        loaded.environments[0].environment,
        parse_environment(FIXTURE).unwrap()
    );
}
