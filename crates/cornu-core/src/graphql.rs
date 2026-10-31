use std::fmt;

use serde::{Deserialize, Deserializer};

use crate::{Body, HttpClient, Method, Request, SendError};

/// Sticks to the June 2018 spec (no `specifiedByUrl`, no deprecated args) so older servers accept it.
pub const INTROSPECTION_QUERY: &str = r#"query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
  }
}

fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes { ...TypeRef }
}

fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
}

fragment TypeRef on __Type {
  kind
  name
  ofType { kind name ofType { kind name ofType { kind name ofType { kind name
    ofType { kind name ofType { kind name ofType { kind name } } } } } } }
}"#;

#[derive(Debug, thiserror::Error)]
pub enum IntrospectionError {
    #[error(transparent)]
    Send(#[from] SendError),
    #[error("the server answered {0}")]
    Status(u16),
    #[error("the server refused the introspection query: {}", .0.join("; "))]
    Refused(Vec<String>),
    #[error("not an introspection result: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TypeKind {
    Scalar,
    Object,
    Interface,
    Union,
    Enum,
    InputObject,
    List,
    NonNull,
}

impl TypeKind {
    pub fn label(self) -> &'static str {
        match self {
            TypeKind::Scalar => "scalar",
            TypeKind::Object => "type",
            TypeKind::Interface => "interface",
            TypeKind::Union => "union",
            TypeKind::Enum => "enum",
            TypeKind::InputObject => "input",
            TypeKind::List => "list",
            TypeKind::NonNull => "non-null",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    #[serde(default, deserialize_with = "type_name")]
    pub query_type: Option<String>,
    #[serde(default, deserialize_with = "type_name")]
    pub mutation_type: Option<String>,
    #[serde(default, deserialize_with = "type_name")]
    pub subscription_type: Option<String>,
    pub types: Vec<FullType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullType {
    pub kind: TypeKind,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub fields: Vec<Field>,
    #[serde(default, deserialize_with = "nullable")]
    pub input_fields: Vec<InputValue>,
    #[serde(default, deserialize_with = "nullable")]
    pub interfaces: Vec<TypeRef>,
    #[serde(default, deserialize_with = "nullable")]
    pub enum_values: Vec<EnumValue>,
    #[serde(default, deserialize_with = "nullable")]
    pub possible_types: Vec<TypeRef>,
}

impl FullType {
    /// `__Type`, `__Schema`… only matter to tools like this one.
    pub fn is_builtin(&self) -> bool {
        self.name.starts_with("__")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub args: Vec<InputValue>,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputValue {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    #[serde(default)]
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumValue {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeRef {
    pub kind: TypeKind,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub of_type: Option<Box<TypeRef>>,
}

impl TypeRef {
    /// The type under the `[…]` and `!` wrappers.
    pub fn named(&self) -> &str {
        match (&self.name, &self.of_type) {
            (Some(name), _) => name,
            (None, Some(inner)) => inner.named(),
            (None, None) => "",
        }
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.kind, &self.of_type) {
            (TypeKind::List, Some(inner)) => write!(f, "[{inner}]"),
            (TypeKind::NonNull, Some(inner)) => write!(f, "{inner}!"),
            _ => f.write_str(self.named()),
        }
    }
}

impl Schema {
    /// Accepts a full `{"data": {"__schema": …}}` response or a bare `{"__schema": …}` dump.
    pub fn parse(text: &str) -> Result<Schema, IntrospectionError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| IntrospectionError::Invalid(error.to_string()))?;
        let schema = value
            .pointer("/data/__schema")
            .or_else(|| value.get("__schema"))
            .filter(|schema| !schema.is_null());
        let Some(schema) = schema else {
            let errors = error_messages(&value);
            return Err(if errors.is_empty() {
                IntrospectionError::Invalid("no __schema in the response".into())
            } else {
                IntrospectionError::Refused(errors)
            });
        };
        Schema::deserialize(schema).map_err(|error| IntrospectionError::Invalid(error.to_string()))
    }

    pub fn type_named(&self, name: &str) -> Option<&FullType> {
        self.types.iter().find(|ty| ty.name == name)
    }

    /// Query, mutation and subscription, as `(operation, type name)`.
    pub fn roots(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("query", &self.query_type),
            ("mutation", &self.mutation_type),
            ("subscription", &self.subscription_type),
        ]
        .into_iter()
        .filter_map(|(operation, name)| Some((operation, name.as_deref()?)))
    }
}

fn error_messages(value: &serde_json::Value) -> Vec<String> {
    value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(
            |error| match error.get("message").and_then(|m| m.as_str()) {
                Some(message) => message.to_owned(),
                None => error.to_string(),
            },
        )
        .collect()
}

/// The same endpoint, headers and auth as `request`, asking for the schema instead.
pub fn introspection_request(request: &Request) -> Request {
    let mut introspection = request.clone();
    if introspection.method == Method::Get {
        introspection.method = Method::Post;
    }
    introspection.body = Body::GraphQl {
        query: INTROSPECTION_QUERY.to_owned(),
        variables: String::new(),
    };
    introspection
}

/// `request` must already be resolved: it is sent as is, with the introspection query as body.
pub async fn introspect(
    client: &HttpClient,
    request: &Request,
) -> Result<Schema, IntrospectionError> {
    let response = client.send(&introspection_request(request)).await?;
    match Schema::parse(&response.text()) {
        Err(IntrospectionError::Invalid(_)) if !response.is_success() => {
            Err(IntrospectionError::Status(response.status))
        }
        result => result,
    }
}

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn type_name<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Named {
        name: String,
    }
    Ok(Option::<Named>::deserialize(deserializer)?.map(|named| named.name))
}
