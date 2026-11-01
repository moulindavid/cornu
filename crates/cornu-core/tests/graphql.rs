use cornu_core::graphql::completion::{SuggestionKind, complete};
use cornu_core::graphql::{IntrospectionError, Schema, TypeKind, introspect};
use cornu_core::{Auth, ClientOptions, HttpClient, Method, Request};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const COUNTRIES: &str = include_str!("fixtures/countries-introspection.json");

fn countries() -> Schema {
    Schema::parse(COUNTRIES).unwrap()
}

#[test]
fn real_introspection_result_is_parsed() {
    let schema = countries();
    assert_eq!(schema.roots().collect::<Vec<_>>(), [("query", "Query")]);
    assert_eq!(schema.types.len(), 23);

    let query = schema.type_named("Query").unwrap();
    assert_eq!(query.kind, TypeKind::Object);
    let country = query.fields.iter().find(|f| f.name == "country").unwrap();
    assert_eq!(country.ty.to_string(), "Country");
    assert_eq!(country.args[0].name, "code");
    assert_eq!(country.args[0].ty.to_string(), "ID!");
    assert_eq!(country.args[0].ty.named(), "ID");

    let countries = query.fields.iter().find(|f| f.name == "countries").unwrap();
    assert_eq!(countries.ty.to_string(), "[Country!]!");
    assert_eq!(countries.ty.named(), "Country");
    assert_eq!(countries.args[0].ty.named(), "CountryFilterInput");

    let filter = schema.type_named("CountryFilterInput").unwrap();
    assert_eq!(filter.kind, TypeKind::InputObject);
    assert!(filter.input_fields.iter().any(|f| f.name == "code"));
    assert!(filter.fields.is_empty());
    assert!(schema.type_named("__Type").unwrap().is_builtin());
}

#[test]
fn enums_unions_and_deprecations() {
    let text = json!({"__schema": {
        "queryType": {"name": "Query"},
        "mutationType": {"name": "Mutation"},
        "subscriptionType": null,
        "types": [
            {"kind": "ENUM", "name": "Role", "enumValues": [
                {"name": "ADMIN", "isDeprecated": false},
                {"name": "ROOT", "isDeprecated": true, "deprecationReason": "use ADMIN"}
            ]},
            {"kind": "UNION", "name": "Result", "possibleTypes": [
                {"kind": "OBJECT", "name": "User"}, {"kind": "OBJECT", "name": "Error"}
            ]}
        ]
    }})
    .to_string();
    let schema = Schema::parse(&text).unwrap();
    assert_eq!(
        schema.roots().collect::<Vec<_>>(),
        [("query", "Query"), ("mutation", "Mutation")]
    );
    let role = schema.type_named("Role").unwrap();
    assert_eq!(
        role.enum_values[1].deprecation_reason.as_deref(),
        Some("use ADMIN")
    );
    assert_eq!(schema.type_named("Result").unwrap().possible_types.len(), 2);
}

#[test]
fn refusals_and_garbage_are_reported() {
    let refused = r#"{"errors": [{"message": "introspection is disabled"}], "data": null}"#;
    assert!(matches!(
        Schema::parse(refused),
        Err(IntrospectionError::Refused(messages)) if messages == ["introspection is disabled"]
    ));
    assert!(matches!(
        Schema::parse(r#"{"data": {"user": null}}"#),
        Err(IntrospectionError::Invalid(_))
    ));
    assert!(matches!(
        Schema::parse("<html>"),
        Err(IntrospectionError::Invalid(_))
    ));
}

#[tokio::test]
async fn introspection_uses_the_request_endpoint_and_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer secret"))
        .and(header("content-type", "application/json"))
        .and(body_string_contains("__schema"))
        .respond_with(ResponseTemplate::new(200).set_body_string(COUNTRIES))
        .mount(&server)
        .await;
    let client = HttpClient::new(&ClientOptions::default()).unwrap();
    let request = Request::new(Method::Get, server.uri()).with_auth(Auth::Bearer {
        token: "secret".into(),
    });
    let schema = introspect(&client, &request).await.unwrap();
    assert_eq!(schema.query_type.as_deref(), Some("Query"));

    let denied = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&denied)
        .await;
    let request = Request::new(Method::Post, denied.uri());
    assert!(matches!(
        introspect(&client, &request).await,
        Err(IntrospectionError::Status(401))
    ));
}

fn suggest(schema: &Schema, marked: &str) -> Vec<String> {
    let cursor = marked.find('|').unwrap();
    let query = marked.replace('|', "");
    complete(schema, &query, cursor)
        .items
        .into_iter()
        .map(|item| item.insert)
        .collect()
}

#[test]
fn fields_follow_the_selection_path() {
    let schema = countries();
    assert_eq!(suggest(&schema, "{ cou| }"), ["countries", "country"]);
    assert_eq!(
        suggest(&schema, "{ countries { languages { na| } } }"),
        ["name", "native", "__typename"],
        "prefix matches first"
    );
    assert!(suggest(&schema, "query Q($c: ID!) { country(code: $c) { cap| } }") == ["capital"]);
    let nested = suggest(&schema, "{ countries { languages { | } } }");
    assert_eq!(
        nested,
        ["code", "countries", "name", "native", "rtl", "__typename"]
    );
    assert_eq!(
        suggest(&schema, "{ countries { first: lang|"),
        ["languages"],
        "aliases"
    );
    assert_eq!(
        suggest(&schema, "{ countries { name continent { code } st| } }"),
        ["states"],
        "back in the parent after a nested selection"
    );
    assert_eq!(
        suggest(&schema, "fragment F on Country { emo| }"),
        ["emoji", "emojiU"]
    );
    assert_eq!(
        suggest(
            &schema,
            "{ country(code: \"FR\") @include(if: true) { pho| } }"
        ),
        ["phone", "phones"]
    );
}

#[test]
fn arguments_type_conditions_and_quiet_places() {
    let schema = countries();
    assert_eq!(suggest(&schema, "{ country(|) }"), ["code: "]);
    assert_eq!(
        suggest(&schema, "{ countries(filter: { code: { eq: \"FR\" } } |) }"),
        Vec::<String>::new()
    );
    assert_eq!(
        suggest(&schema, "{ countries { ... on Cont| } }"),
        ["Continent"]
    );
    for quiet in [
        "{ country(code: \"cou|",
        "# cou|",
        "{ country(code: $co|",
        "{ countries { 1|",
        "{ countries { name|",
    ] {
        assert!(suggest(&schema, quiet).is_empty(), "{quiet}");
    }
    let completion = complete(&schema, "{ cou }", 5);
    assert_eq!(completion.start, 2);
    assert_eq!(completion.items[0].detail, "[Country!]!");
    assert_eq!(completion.items[0].kind, SuggestionKind::Field);
}

#[test]
fn enum_arguments_and_mutations() {
    let text = json!({"__schema": {
        "queryType": {"name": "Query"},
        "mutationType": {"name": "Mutation"},
        "types": [
            {"kind": "OBJECT", "name": "Query", "fields": []},
            {"kind": "OBJECT", "name": "Mutation", "fields": [
                {"name": "setRole", "type": {"kind": "SCALAR", "name": "Boolean"}, "args": [
                    {"name": "user", "type": {"kind": "SCALAR", "name": "ID"}},
                    {"name": "role", "type": {"kind": "NON_NULL", "ofType": {"kind": "ENUM", "name": "Role"}}}
                ]}
            ]},
            {"kind": "ENUM", "name": "Role", "enumValues": [
                {"name": "ADMIN"}, {"name": "ROOT", "isDeprecated": true}
            ]}
        ]
    }})
    .to_string();
    let schema = Schema::parse(&text).unwrap();
    assert_eq!(suggest(&schema, "mutation { set| }"), ["setRole"]);
    assert_eq!(
        suggest(&schema, "mutation { setRole(user: \"1\" |) }"),
        ["role: "]
    );
    assert_eq!(
        suggest(&schema, "mutation { setRole(role: |) }"),
        ["ADMIN", "ROOT"]
    );
    assert_eq!(suggest(&schema, "mutation { setRole(role: R|) }"), ["ROOT"]);
}
