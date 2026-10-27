use std::fs;
use std::path::{Path, PathBuf};

use cornu_core::{Auth, Body, Environment, KeyValue, Method, Request};
use cornu_storage::{
    CONFIG_FILE, EnvironmentEntry, FOLDER_FILE, Folder, Item, RequestEntry, Settings, StorageError,
    Workspace,
};

fn workspace() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(dir.path(), "Demo API").unwrap();
    (dir, workspace)
}

fn read(root: &Path, path: impl AsRef<Path>) -> String {
    fs::read_to_string(root.join(path)).unwrap()
}

/// Root: `health` request and a `Users` folder (Bearer auth) holding two requests.
fn populate(workspace: &mut Workspace) {
    let users_path = workspace.new_folder_path(Path::new(""), "Users");
    let list = RequestEntry {
        path: workspace.new_request_path(&users_path, "List users"),
        request: Request::new(Method::Get, "{{base_url}}/users")
            .with_name("List users")
            .with_query("page", "1"),
    };
    let create = RequestEntry {
        path: workspace.new_request_path(&users_path, "Create user"),
        request: Request::new(Method::Post, "{{base_url}}/users")
            .with_name("Create user")
            .with_body(Body::Json {
                text: "{\n  \"name\": \"{{name}}\"\n}".into(),
            }),
    };
    let health = RequestEntry {
        path: workspace.new_request_path(Path::new(""), "Health"),
        request: Request::new(Method::Get, "{{base_url}}/health").with_name("Health"),
    };
    let users = Folder {
        path: users_path,
        settings: Settings {
            name: "Users".into(),
            variables: vec![KeyValue::new("resource", "users")],
            auth: Auth::Bearer {
                token: "{{token}}".into(),
            },
            order: Vec::new(),
        },
        items: vec![Item::Request(create), Item::Request(list)],
    };
    workspace.tree.settings.variables = vec![
        KeyValue::new("base_url", "https://global.example"),
        KeyValue::new("api_key", "global-secret").secret(),
    ];
    workspace.tree.items = vec![Item::Folder(users), Item::Request(health)];

    save_all(workspace, &workspace.tree.clone());
    let dev = EnvironmentEntry {
        path: workspace.new_environment_path("Dev"),
        environment: Environment::new("Dev")
            .with_variable("base_url", "https://dev.example")
            .with_variable("token", "dev-token"),
    };
    let mut dev = dev;
    dev.environment.variables[1].secret = true;
    workspace.save_environment(&dev).unwrap();
    workspace.environments = vec![dev];
}

fn save_all(workspace: &Workspace, folder: &Folder) {
    workspace.save_folder(folder).unwrap();
    for item in &folder.items {
        match item {
            Item::Folder(child) => save_all(workspace, child),
            Item::Request(entry) => workspace.save_request(entry).unwrap(),
        }
    }
}

#[test]
fn init_creates_config_and_gitignore() {
    let (dir, workspace) = workspace();
    assert_eq!(workspace.tree.settings.name, "Demo API");
    assert!(dir.path().join("environments").is_dir());
    assert!(read(dir.path(), ".gitignore").contains(".cornu/"));

    Workspace::init(dir.path(), "ignored").unwrap();
    assert_eq!(read(dir.path(), ".gitignore").matches(".cornu/").count(), 1);
    assert_eq!(
        Workspace::load(dir.path()).unwrap().tree.settings.name,
        "Demo API"
    );
}

#[test]
fn init_appends_to_an_existing_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".gitignore"), "target").unwrap();
    Workspace::init(dir.path(), "x").unwrap();
    assert!(read(dir.path(), ".gitignore").starts_with("target\n"));
}

#[test]
fn load_rejects_a_directory_without_config() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        Workspace::load(dir.path()),
        Err(StorageError::NotAWorkspace(_))
    ));
}

#[test]
fn workspace_round_trips_through_disk() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);

    let loaded = Workspace::load(dir.path()).unwrap();
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.tree, workspace.tree);
    assert_eq!(loaded.environments, workspace.environments);
}

#[test]
fn layout_is_one_file_per_request() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let root = dir.path();

    assert!(root.join(CONFIG_FILE).is_file());
    assert!(root.join("health.req.toml").is_file());
    assert!(root.join("users").join(FOLDER_FILE).is_file());
    assert!(root.join("users/list-users.req.toml").is_file());
    assert!(root.join("users/create-user.req.toml").is_file());
    assert!(root.join("environments/dev.toml").is_file());

    let folder = read(root, "users/_folder.toml");
    assert!(
        folder.contains("order = [") && folder.contains("\"create-user\""),
        "{folder}"
    );
}

#[test]
fn item_order_is_preserved() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let loaded = Workspace::load(dir.path()).unwrap();
    let Item::Folder(users) = &loaded.tree.items[0] else {
        panic!("expected the Users folder first");
    };
    let names: Vec<_> = users.items.iter().map(Item::entry_name).collect();
    assert_eq!(names, ["create-user", "list-users"]);
}

#[test]
fn multiline_bodies_stay_readable_in_diffs() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let file = read(dir.path(), "users/create-user.req.toml");
    assert!(file.contains("\"\"\""), "{file}");
    assert!(file.contains("  \"name\": \"{{name}}\""), "{file}");
}

#[test]
fn secrets_never_reach_shared_files() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let root = dir.path();

    assert!(!read(root, "environments/dev.toml").contains("dev-token"));
    assert!(!read(root, CONFIG_FILE).contains("global-secret"));
    let secrets = read(root, ".cornu/secrets.toml");
    assert!(secrets.contains("dev-token") && secrets.contains("global-secret"));
    assert!(read(root, "environments/dev.toml").contains("secret = true"));
}

#[test]
fn saving_twice_produces_identical_files() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let snapshot = |root: &Path| -> Vec<(PathBuf, String)> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_owned()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                } else {
                    files.push((entry.path(), fs::read_to_string(entry.path()).unwrap()));
                }
            }
        }
        files.sort();
        files
    };
    let before = snapshot(dir.path());
    let loaded = Workspace::load(dir.path()).unwrap();
    save_all(&loaded, &loaded.tree);
    for env in &loaded.environments {
        loaded.save_environment(env).unwrap();
    }
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn broken_files_are_reported_without_blocking_the_rest() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    fs::write(dir.path().join("users/broken.req.toml"), "method = [").unwrap();

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].contains("broken.req.toml"));
    let Item::Folder(users) = &loaded.tree.items[0] else {
        panic!("expected the Users folder");
    };
    assert_eq!(users.items.len(), 2);
}

#[test]
fn folders_without_settings_use_their_directory_name() {
    let (dir, _) = workspace();
    fs::create_dir(dir.path().join("orders")).unwrap();
    fs::write(
        dir.path().join("orders/list.req.toml"),
        "name = \"List\"\nurl = \"/orders\"\n",
    )
    .unwrap();

    let loaded = Workspace::load(dir.path()).unwrap();
    let Item::Folder(orders) = &loaded.tree.items[0] else {
        panic!("expected the orders folder");
    };
    assert_eq!(orders.settings.name, "orders");
    assert_eq!(orders.items.len(), 1);
}

#[test]
fn new_paths_avoid_existing_names() {
    let (_dir, mut workspace) = workspace();
    populate(&mut workspace);
    assert_eq!(
        workspace.new_request_path(Path::new(""), "Health"),
        PathBuf::from("health-2.req.toml")
    );
    assert_eq!(
        workspace.new_folder_path(Path::new(""), "users"),
        PathBuf::from("users-2")
    );
    assert_eq!(
        workspace.new_environment_path("dev"),
        PathBuf::from("environments/dev-2.toml")
    );
}

#[test]
fn auth_is_inherited_from_folders() {
    let (_dir, mut workspace) = workspace();
    populate(&mut workspace);
    let list = workspace
        .find_request(Path::new("users/list-users.req.toml"))
        .unwrap();
    assert_eq!(
        workspace.effective_auth(&list.path, &list.request.auth),
        Auth::Bearer {
            token: "{{token}}".into()
        }
    );

    let health = workspace
        .find_request(Path::new("health.req.toml"))
        .unwrap();
    assert_eq!(
        workspace.effective_auth(&health.path, &health.request.auth),
        Auth::None
    );

    assert_eq!(
        workspace.effective_auth(&list.path, &Auth::None),
        Auth::None
    );
}

#[test]
fn variables_follow_postman_precedence() {
    let (_dir, mut workspace) = workspace();
    populate(&mut workspace);
    let path = Path::new("users/list-users.req.toml");

    let without_env = workspace.variables(path, None);
    assert_eq!(without_env.get("base_url"), Some("https://global.example"));
    assert_eq!(without_env.get("resource"), Some("users"));

    let dev = &workspace.environments[0].environment;
    let with_env = workspace.variables(path, Some(dev));
    assert_eq!(with_env.get("base_url"), Some("https://dev.example"));
    assert_eq!(with_env.get("token"), Some("dev-token"));
    assert_eq!(with_env.get("resource"), Some("users"));

    let root_vars = workspace.variables(Path::new("health.req.toml"), None);
    assert_eq!(root_vars.get("resource"), None);
}

fn item_names(folder: &Folder) -> Vec<String> {
    folder.items.iter().map(Item::entry_name).collect()
}

#[test]
fn created_items_are_written_and_appended() {
    let (dir, mut workspace) = workspace();
    let folder = workspace.create_folder(Path::new(""), "Orders").unwrap();
    let request = workspace
        .create_request(
            &folder,
            Request::new(Method::Get, "/orders").with_name("List"),
        )
        .unwrap();
    assert_eq!(request, PathBuf::from("orders/list.req.toml"));
    assert!(dir.path().join(&request).is_file());

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
}

#[test]
fn renaming_a_folder_moves_it_and_its_requests() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let new_path = workspace.rename(Path::new("users"), "People").unwrap();
    assert_eq!(new_path, PathBuf::from("people"));
    assert!(!dir.path().join("users").exists());
    assert!(dir.path().join("people/list-users.req.toml").is_file());
    assert!(
        workspace
            .find_request(Path::new("people/list-users.req.toml"))
            .is_some()
    );

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
    assert_eq!(item_names(&loaded.tree), ["people", "health"]);
}

#[test]
fn renaming_to_the_same_slug_keeps_the_file() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let path = Path::new("health.req.toml");
    assert_eq!(workspace.rename(path, "HEALTH").unwrap(), path);
    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.find_request(path).unwrap().request.name, "HEALTH");
}

#[test]
fn duplicating_places_the_copy_after_the_original() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let copy = workspace.duplicate(Path::new("users")).unwrap();
    assert_eq!(copy, PathBuf::from("users-copy"));
    assert!(dir.path().join("users-copy/create-user.req.toml").is_file());

    let request = workspace.duplicate(Path::new("health.req.toml")).unwrap();
    assert_eq!(
        workspace.find_request(&request).unwrap().request.name,
        "Health copy"
    );

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
    assert_eq!(
        item_names(&loaded.tree),
        ["users", "users-copy", "health", "health-copy"]
    );
}

#[test]
fn deleting_removes_files_and_order_entries() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    workspace.delete(Path::new("users")).unwrap();
    workspace.delete(Path::new("health.req.toml")).unwrap();
    assert!(!dir.path().join("users").exists());
    assert!(!dir.path().join("health.req.toml").exists());
    assert!(!read(dir.path(), CONFIG_FILE).contains("users"));
    assert!(Workspace::load(dir.path()).unwrap().tree.items.is_empty());
}

#[test]
fn updates_are_saved_in_place() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let path = Path::new("users/list-users.req.toml");
    let mut request = workspace.find_request(path).unwrap().request.clone();
    request.url = "{{base_url}}/v2/users".into();
    workspace.update_request(path, request).unwrap();

    let mut settings = workspace
        .folder(Path::new("users"))
        .unwrap()
        .settings
        .clone();
    settings.auth = Auth::None;
    workspace
        .update_folder(Path::new("users"), settings)
        .unwrap();

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
    assert!(read(dir.path(), "users/list-users.req.toml").contains("/v2/users"));
}

#[test]
fn environments_can_be_created_and_deleted() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let index = workspace.create_environment("Prod").unwrap();
    assert!(dir.path().join("environments/prod.toml").is_file());

    workspace.delete_environment(index).unwrap();
    workspace.delete_environment(0).unwrap();
    assert!(!dir.path().join("environments/dev.toml").exists());
    assert!(!read(dir.path(), ".cornu/secrets.toml").contains("dev-token"));
    assert!(Workspace::load(dir.path()).unwrap().environments.is_empty());
}

fn add_folder_secret(workspace: &mut Workspace, path: &str) {
    let path = Path::new(path);
    let mut settings = workspace.folder(path).unwrap().settings.clone();
    settings
        .variables
        .push(KeyValue::new("folder_token", "folder-secret").secret());
    workspace.update_folder(path, settings).unwrap();
}

#[test]
fn folder_secrets_stay_out_of_shared_files() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    add_folder_secret(&mut workspace, "users");

    let shared = read(dir.path(), "users/_folder.toml");
    assert!(
        !shared.contains("folder-secret") && shared.contains("secret = true"),
        "{shared}"
    );
    let secrets = read(dir.path(), ".cornu/secrets.toml");
    assert!(secrets.contains("[folders.users]"), "{secrets}");

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
    let variables = loaded.variables(Path::new("users/list-users.req.toml"), None);
    assert_eq!(variables.get("folder_token"), Some("folder-secret"));
}

#[test]
fn folder_secrets_follow_renames_of_the_folder_and_its_parents() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    let nested = workspace
        .create_folder(Path::new("users"), "Admin")
        .unwrap();
    add_folder_secret(&mut workspace, "users");
    add_folder_secret(&mut workspace, nested.to_str().unwrap());

    workspace.rename(Path::new("users"), "People").unwrap();
    let secrets = read(dir.path(), ".cornu/secrets.toml");
    assert!(!secrets.contains("[folders.users"), "{secrets}");
    assert!(secrets.contains("[folders.\"people/admin\"]"), "{secrets}");

    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
    let admin = loaded.folder(Path::new("people/admin")).unwrap();
    assert_eq!(admin.settings.variables[0].value, "folder-secret");
}

#[test]
fn duplicated_and_deleted_folders_update_their_secrets() {
    let (dir, mut workspace) = workspace();
    populate(&mut workspace);
    add_folder_secret(&mut workspace, "users");

    workspace.duplicate(Path::new("users")).unwrap();
    assert!(read(dir.path(), ".cornu/secrets.toml").contains("[folders.users-copy]"));

    workspace.delete(Path::new("users")).unwrap();
    let secrets = read(dir.path(), ".cornu/secrets.toml");
    assert!(!secrets.contains("[folders.users]"), "{secrets}");
    let loaded = Workspace::load(dir.path()).unwrap();
    assert_eq!(loaded.tree, workspace.tree);
}
