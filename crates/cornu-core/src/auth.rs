use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    #[default]
    Inherit,
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        #[serde(default)]
        password: String,
    },
    ApiKey {
        key: String,
        value: String,
        #[serde(default)]
        location: ApiKeyLocation,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

impl Auth {
    pub fn is_inherit(&self) -> bool {
        matches!(self, Auth::Inherit)
    }

    /// `chain` goes from the most specific level (the request) to the least (the collection).
    pub fn effective<'a>(chain: impl IntoIterator<Item = &'a Auth>) -> Auth {
        chain
            .into_iter()
            .find(|auth| !auth.is_inherit())
            .cloned()
            .unwrap_or(Auth::None)
    }

    pub fn header(&self) -> Option<(String, String)> {
        match self {
            Auth::Bearer { token } => Some(("Authorization".into(), format!("Bearer {token}"))),
            Auth::Basic { username, password } => {
                let credentials = STANDARD.encode(format!("{username}:{password}"));
                Some(("Authorization".into(), format!("Basic {credentials}")))
            }
            Auth::ApiKey {
                key,
                value,
                location: ApiKeyLocation::Header,
            } => Some((key.clone(), value.clone())),
            _ => None,
        }
    }

    pub fn query(&self) -> Option<(&str, &str)> {
        match self {
            Auth::ApiKey {
                key,
                value,
                location: ApiKeyLocation::Query,
            } => Some((key, value)),
            _ => None,
        }
    }

    pub(crate) fn fields_mut(&mut self) -> Vec<&mut String> {
        match self {
            Auth::Inherit | Auth::None => Vec::new(),
            Auth::Bearer { token } => vec![token],
            Auth::Basic { username, password } => vec![username, password],
            Auth::ApiKey { key, value, .. } => vec![key, value],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_takes_the_most_specific_concrete_auth() {
        let collection = Auth::Bearer {
            token: "{{token}}".into(),
        };
        let folder = Auth::Inherit;
        let request = Auth::Inherit;
        assert_eq!(
            Auth::effective([&request, &folder, &collection]),
            collection
        );

        let opted_out = Auth::None;
        assert_eq!(
            Auth::effective([&opted_out, &folder, &collection]),
            Auth::None
        );
        assert_eq!(Auth::effective([&Auth::Inherit]), Auth::None);
    }

    #[test]
    fn bearer_and_basic_build_authorization_headers() {
        let bearer = Auth::Bearer {
            token: "abc".into(),
        };
        assert_eq!(
            bearer.header(),
            Some(("Authorization".into(), "Bearer abc".into()))
        );
        let basic = Auth::Basic {
            username: "Aladdin".into(),
            password: "open sesame".into(),
        };
        assert_eq!(
            basic.header(),
            Some((
                "Authorization".into(),
                "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==".into()
            ))
        );
    }

    #[test]
    fn api_key_goes_to_header_or_query() {
        let mut auth = Auth::ApiKey {
            key: "X-Api-Key".into(),
            value: "k".into(),
            location: ApiKeyLocation::Header,
        };
        assert_eq!(auth.header(), Some(("X-Api-Key".into(), "k".into())));
        assert_eq!(auth.query(), None);

        if let Auth::ApiKey { location, .. } = &mut auth {
            *location = ApiKeyLocation::Query;
        }
        assert_eq!(auth.header(), None);
        assert_eq!(auth.query(), Some(("X-Api-Key", "k")));
    }

    #[test]
    fn serializes_as_tagged_toml_table() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            auth: Auth,
        }
        let text = "[auth]\ntype = \"api_key\"\nkey = \"key\"\nvalue = \"{{api_key}}\"\n";
        let parsed: Wrapper = toml::from_str(text).unwrap();
        assert_eq!(
            parsed.auth,
            Auth::ApiKey {
                key: "key".into(),
                value: "{{api_key}}".into(),
                location: ApiKeyLocation::Header,
            }
        );
        let round_trip: Wrapper = toml::from_str(&toml::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(round_trip, parsed);
    }
}
