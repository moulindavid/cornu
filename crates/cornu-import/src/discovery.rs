//! Finds the OpenAPI spec of an API from its base URL or its docs page.

use cornu_core::{HttpClient, Method, Request, SendError};

use crate::Collection;
use crate::openapi::parse_spec;

/// Where frameworks usually serve their spec: FastAPI, springdoc, Swashbuckle, swaggo, .NET 9…
const WELL_KNOWN: [&str; 15] = [
    "openapi.json",
    "openapi.yaml",
    "swagger.json",
    "swagger.yaml",
    "v3/api-docs",
    "v2/api-docs",
    "api-docs",
    "swagger/v1/swagger.json",
    "openapi/v1.json",
    "swagger/doc.json",
    "swagger/docs/v1",
    "api/openapi.json",
    "api/swagger.json",
    "docs/openapi.json",
    ".well-known/openapi.json",
];

#[derive(Debug)]
pub struct Discovered {
    pub url: String,
    pub collection: Collection,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("invalid URL {0:?}")]
    InvalidUrl(String),
    #[error(transparent)]
    Unreachable(SendError),
    #[error("no OpenAPI spec found ({tried} locations tried)")]
    NotFound { tried: usize },
}

/// Tries `url` itself, a spec linked from it if it is a docs page, then the usual locations.
pub async fn discover(client: &HttpClient, url: &str) -> Result<Discovered, DiscoveryError> {
    let url = url.trim();
    let start = with_scheme(url);
    let parsed = url::Url::parse(&start).map_err(|_| DiscoveryError::InvalidUrl(url.to_owned()))?;

    let mut tried = vec![start.clone()];
    let page = match fetch(client, &start).await {
        Err(error) => return Err(DiscoveryError::Unreachable(error)),
        Ok(page) => page.unwrap_or_default(),
    };
    if let Ok(collection) = parse_spec(&page, Some(&start)) {
        return Ok(Discovered {
            url: start,
            collection,
        });
    }

    let mut linked = linked_specs(&page, &parsed);
    for script in initializer_scripts(&page, &parsed) {
        if let Ok(Some(code)) = fetch(client, &script).await {
            linked.extend(linked_specs(&code, &parsed));
        }
    }
    for candidate in linked.into_iter().chain(candidates(&parsed)) {
        if tried.contains(&candidate) {
            continue;
        }
        tried.push(candidate.clone());
        if let Some(found) = try_spec(client, &candidate).await {
            return Ok(found);
        }
    }
    Err(DiscoveryError::NotFound { tried: tried.len() })
}

async fn try_spec(client: &HttpClient, url: &str) -> Option<Discovered> {
    let body = fetch(client, url).await.ok()??;
    let collection = parse_spec(&body, Some(url)).ok()?;
    Some(Discovered {
        url: url.to_owned(),
        collection,
    })
}

/// `Ok(None)` for a non-2xx answer: the server is there, the spec is not.
async fn fetch(client: &HttpClient, url: &str) -> Result<Option<String>, SendError> {
    let request = Request::new(Method::Get, url).with_header(
        "Accept",
        "application/json, application/yaml;q=0.9, */*;q=0.8",
    );
    let response = client.send(&request).await?;
    Ok((200..300)
        .contains(&response.status)
        .then(|| String::from_utf8_lossy(&response.body).into_owned()))
}

fn with_scheme(url: &str) -> String {
    if url.contains("://") {
        url.to_owned()
    } else {
        format!("https://{url}")
    }
}

/// The well-known paths under the URL's own path, then under each parent up to the origin.
pub fn candidates(url: &url::Url) -> Vec<String> {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let mut roots = Vec::new();
    let mut path = url.path().trim_end_matches('/').to_owned();
    loop {
        roots.push(format!("{path}/"));
        match path.rfind('/') {
            Some(slash) => path.truncate(slash),
            None => break,
        }
    }

    let mut candidates: Vec<String> = Vec::new();
    for root in roots {
        url.set_path(&root);
        for known in WELL_KNOWN {
            if let Ok(candidate) = url.join(known) {
                let candidate = candidate.to_string();
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
    }
    candidates
}

/// Spec-looking URLs in a docs page or script, same host first.
///
/// Swagger UI, Redoc and Scalar all name the spec somewhere in their page or initializer,
/// but not always as a plain `url: "…"`, so any string shaped like a spec URL counts.
pub fn linked_specs(text: &str, page: &url::Url) -> Vec<String> {
    let mut same_host = Vec::new();
    let mut other_hosts = Vec::new();
    for token in text.split(is_separator) {
        let lower = token.to_ascii_lowercase();
        let path = lower.split(['?', '#']).next().unwrap_or_default();
        let located =
            lower.starts_with("http") || lower.starts_with('/') || lower.starts_with("./");
        let spec_like = [".json", ".yaml", ".yml"]
            .iter()
            .any(|suffix| path.ends_with(suffix))
            || path.contains("api-docs");
        if !located || !spec_like || path.ends_with("package.json") {
            continue;
        }
        let Ok(url) = page.join(token) else {
            continue;
        };
        let list = if url.host_str() == page.host_str() {
            &mut same_host
        } else {
            &mut other_hosts
        };
        let url = url.to_string();
        if !list.contains(&url) {
            list.push(url);
        }
    }
    same_host.extend(other_hosts);
    same_host
}

/// Recent Swagger UI moved its configuration out of the page into `swagger-initializer.js`.
fn initializer_scripts(page: &str, url: &url::Url) -> Vec<String> {
    page.split(is_separator)
        .filter(|token| {
            let lower = token.to_ascii_lowercase();
            lower.ends_with(".js") && lower.contains("initializer")
        })
        .filter_map(|token| url.join(token).ok())
        .map(String::from)
        .collect()
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || "\"'`,=()<>;[]{}".contains(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_walk_up_to_the_origin() {
        let url = url::Url::parse("https://api.example/v1/docs?x=1#top").unwrap();
        let candidates = candidates(&url);
        assert_eq!(candidates[0], "https://api.example/v1/docs/openapi.json");
        assert!(candidates.contains(&"https://api.example/v1/v3/api-docs".to_owned()));
        assert_eq!(
            candidates.last().unwrap(),
            "https://api.example/.well-known/openapi.json"
        );
        // `/v1/` + `docs/openapi.json` is also `/v1/docs/` + `openapi.json`.
        assert_eq!(candidates.len(), WELL_KNOWN.len() * 3 - 1);
    }

    #[test]
    fn spec_links_are_found_in_docs_pages() {
        let page = url::Url::parse("https://api.example/docs").unwrap();
        let fastapi = r#"<script>
            const ui = SwaggerUIBundle({
                url: '/openapi.json',
                dom_id: '#swagger-ui',
            })</script>"#;
        assert_eq!(
            linked_specs(fastapi, &page),
            ["https://api.example/openapi.json"]
        );

        let redoc = r#"<redoc spec-url="https://cdn.example/spec.yaml"></redoc>"#;
        assert_eq!(
            linked_specs(redoc, &page),
            ["https://cdn.example/spec.yaml"]
        );

        let springdoc = r#"{"urls": [{"url": "/v3/api-docs/public", "name": "public"}]}"#;
        assert_eq!(
            linked_specs(springdoc, &page),
            ["https://api.example/v3/api-docs/public"]
        );

        // Petstore's initializer picks the spec by host at runtime.
        let initializer = r#"const fallback = "https://other.example/v2/swagger.json";
            const services = `other.example=https://other.example/v2/swagger.json,api.example=https://api.example/api/v3/openapi.json`;"#;
        assert_eq!(
            linked_specs(initializer, &page),
            [
                "https://api.example/api/v3/openapi.json",
                "https://other.example/v2/swagger.json"
            ]
        );

        assert!(
            linked_specs(r#"<script src="./swagger-ui-bundle.js"></script>"#, &page).is_empty()
        );
    }

    #[test]
    fn initializer_scripts_are_followed() {
        let page = url::Url::parse("https://api.example/docs/").unwrap();
        let html = r#"<script src="./swagger-ui-bundle.js"></script>
            <script src="./swagger-initializer.js" charset="UTF-8"></script>"#;
        assert_eq!(
            initializer_scripts(html, &page),
            ["https://api.example/docs/swagger-initializer.js"]
        );
    }
}
