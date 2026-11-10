//! `GET /api/openapi.json` — the OpenAPI 3.0 contract of this build.
//!
//! The document lives at the repository root (`openapi.json`) and is embedded
//! in the binary at compile time, so it always describes the routes of the
//! build that serves it. Two guarantees are enforced by tests instead of
//! discipline:
//!
//! - [`document_json`] overwrites `info.version` with `CARGO_PKG_VERSION`
//!   (the same source as the `fleett_build_info` metric and `GET /api/health`),
//!   so a stale literal in the file can never be served;
//! - the unit tests below scan `api_routes` in `mod.rs` and fail the build when
//!   a router route is missing from the document, or when the document claims
//!   an operation the router does not serve.

use std::sync::OnceLock;

/// Raw document embedded from the repository root.
pub const DOCUMENT: &str = include_str!("../../../openapi.json");

/// Parsed document with the binary version injected, serialized once.
static SERVED: OnceLock<String> = OnceLock::new();

/// The exact bytes served by `GET /api/openapi.json`.
///
/// The embedded file is parsed once, `info.version` is overwritten with
/// `env!("CARGO_PKG_VERSION")` and the document is serialized again. The
/// endpoint stays public and the result is always valid JSON.
pub fn document_json() -> &'static str {
    SERVED.get_or_init(|| with_binary_version(DOCUMENT))
}

/// Parse `raw`, overwrite `info.version` with `CARGO_PKG_VERSION` and serialize
/// the result. Split out so the version injection can be tested against a
/// document whose stored version deliberately differs.
fn with_binary_version(raw: &str) -> String {
    let mut doc: serde_json::Value =
        serde_json::from_str(raw).expect("openapi.json must be valid JSON");
    let info = doc
        .get_mut("info")
        .and_then(serde_json::Value::as_object_mut)
        .expect("openapi.json must have an info object");
    info.insert(
        "version".to_string(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    serde_json::to_string(&doc).expect("serializing the document cannot fail")
}

/// Serve the document. Public: it contains no secret and lets SDK generators
/// and integration authors read the contract without a token. Every route it
/// describes still requires authentication as documented.
pub async fn document() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        document_json(),
    )
}

#[cfg(test)]
mod tests {
    use super::{document_json, DOCUMENT};
    use serde_json::Value;
    use std::collections::BTreeSet;

    #[test]
    fn the_document_is_valid_and_covers_every_api_path() {
        let doc: Value = serde_json::from_str(DOCUMENT).expect("openapi.json must be valid JSON");
        assert!(doc["openapi"]
            .as_str()
            .unwrap_or_default()
            .starts_with("3.0"));
        assert!(doc["info"]["title"].is_string());
        let paths = doc["paths"].as_object().expect("paths object");
        assert!(paths.len() > 150, "only {} paths documented", paths.len());
        for (path, operations) in paths {
            assert!(path.starts_with("/api"), "unexpected path {path}");
            let operations = operations.as_object().expect("operation object");
            assert!(!operations.is_empty(), "{path} has no operation");
            for (method, operation) in operations {
                let responses = operation["responses"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{method} {path} has no responses"));
                assert!(
                    !responses.is_empty(),
                    "{method} {path} has an empty responses object"
                );
                assert!(
                    operation["operationId"].is_string(),
                    "{method} {path} has no operationId"
                );
                assert!(
                    operation["tags"]
                        .as_array()
                        .is_some_and(|tags| !tags.is_empty()),
                    "{method} {path} has no tag"
                );
            }
        }
    }

    #[test]
    fn the_served_version_is_the_binary_version() {
        let served: Value = serde_json::from_str(document_json()).expect("served document");
        assert_eq!(
            served["info"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "GET /api/openapi.json must advertise the running binary version"
        );
    }

    #[test]
    fn the_version_is_injected_regardless_of_the_stored_value() {
        let served: Value = serde_json::from_str(&super::with_binary_version(
            r#"{"openapi":"3.0.3","info":{"title":"t","version":"0.0.0-stale"},"paths":{}}"#,
        ))
        .expect("served document");
        assert_eq!(
            served["info"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "the stored version must never be served"
        );
        assert_eq!(served["info"]["title"], "t");
    }

    #[test]
    fn the_embedded_document_version_stays_in_sync() {
        let embedded: Value = serde_json::from_str(DOCUMENT).expect("openapi.json");
        assert_eq!(
            embedded["info"]["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "the version stored in openapi.json must match CARGO_PKG_VERSION"
        );
    }

    /// Every couple `(METHOD, /api/path)` declared by `api_routes` and by the
    /// embedded document must match exactly, in both directions.
    #[test]
    fn every_router_route_is_documented_and_nothing_else_is() {
        let router = router_routes();
        assert!(
            router.len() > 200,
            "only {} router operations parsed: the scanner is probably broken",
            router.len()
        );

        let doc: Value = serde_json::from_str(DOCUMENT).expect("openapi.json");
        let mut documented = BTreeSet::new();
        for (path, operations) in doc["paths"].as_object().expect("paths object") {
            assert!(path.starts_with("/api"), "unexpected path {path}");
            for method in operations.as_object().expect("operation object").keys() {
                documented.insert((method.to_ascii_uppercase(), normalize_path(path)));
            }
        }

        let missing: Vec<String> = router
            .difference(&documented)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        let stale: Vec<String> = documented
            .difference(&router)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "OpenAPI drift between the router and openapi.json.\n\
             Routes served by the router but missing from the document:\n  {}\n\
             Operations documented but absent from the router (stale entries):\n  {}",
            if missing.is_empty() {
                "(none)".to_string()
            } else {
                missing.join("\n  ")
            },
            if stale.is_empty() {
                "(none)".to_string()
            } else {
                stale.join("\n  ")
            }
        );
    }

    /// Axum and OpenAPI both spell path parameters `{name}`; accept the legacy
    /// axum `:name` spelling too so a route can never drift by syntax only.
    fn normalize_path(path: &str) -> String {
        path.split('/')
            .map(|segment| match segment.strip_prefix(':') {
                Some(name) => format!("{{{name}}}"),
                None => segment.to_string(),
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Scan only the `fn api_routes(…)` region of `mod.rs` (up to the fallback
    /// handler) and return every `(METHOD, /api/path)` it declares.
    ///
    /// Handlers are read from the `.route("<literal>", <expression>)` argument,
    /// where the expression may chain `.post(…)`, `.patch(…)`, an explicit
    /// `axum::routing::patch(…)` or a `.layer(…)`. Method names are matched on
    /// identifier boundaries so `target(` never counts as `get(`.
    fn router_routes() -> BTreeSet<(String, String)> {
        let source = include_str!("mod.rs");
        let start = source
            .find("fn api_routes(")
            .expect("mod.rs must define api_routes");
        let end = start
            + source[start..]
                .find("async fn api_not_found")
                .expect("api_routes must be followed by api_not_found");
        let region = &source[start..end];
        let bytes = region.as_bytes();
        let mut routes = BTreeSet::new();
        let mut cursor = 0;

        while let Some(found) = region[cursor..].find(".route(") {
            let open = cursor + found + ".route".len();
            let mut i = open + 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            assert_eq!(
                bytes[i],
                b'"',
                "expected a path literal after .route( in api_routes: {:?}",
                &region[open..(open + 60).min(region.len())]
            );
            let literal_start = i + 1;
            let literal_end = literal_start
                + region[literal_start..]
                    .find('"')
                    .expect("unterminated route path");
            let path = format!("/api{}", &region[literal_start..literal_end]);

            // Find the matching close parenthesis of `.route(`.
            let mut depth = 1usize;
            let mut j = open + 1;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            assert_eq!(depth, 0, "unbalanced .route( for {path}");
            let expression = &region[(open + 1)..(j - 1)];

            for method in ["get", "post", "put", "patch", "delete"] {
                if calls(expression, method) {
                    routes.insert((method.to_ascii_uppercase(), path.clone()));
                }
            }
            cursor = j;
        }

        routes
    }

    /// `true` when `expression` contains a `method(` call, ignoring longer
    /// identifiers that merely end with the method name.
    fn calls(expression: &str, method: &str) -> bool {
        let needle = format!("{method}(");
        let bytes = expression.as_bytes();
        let mut from = 0;
        while let Some(found) = expression[from..].find(&needle) {
            let at = from + found;
            let preceded_by_identifier =
                at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
            if !preceded_by_identifier {
                return true;
            }
            from = at + needle.len();
        }
        false
    }
}
