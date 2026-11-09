//! Integration tests for the audit trail (issue #44): filters, signed JSONL
//! export and retention purge.
//!
//! Every test runs against `MockDriver`; no test ever calls Docker.

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use fleett::api::{self, AppState};
use fleett::config::Config;
use fleett::db::{AuditFilter, Db, NewAuditRecord};
use fleett::driver::mock::MockDriver;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";
const AUDIT_KEY: &[u8] = b"audit-test-key";

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(|_| {})
    }

    fn with_audit_key() -> Self {
        Self::with_config(|cfg| {
            cfg.audit_hmac_key = Some(fleett::audit::AuditKey::new(AUDIT_KEY.to_vec()))
        })
    }

    fn with_config(configure: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cfg = Config::for_tests(dir.path().to_path_buf());
        configure(&mut cfg);
        let state = AppState::bootstrap(cfg, Arc::new(MockDriver::new())).expect("bootstrap");
        Self { _dir: dir, state }
    }

    fn db(&self) -> &Db {
        &self.state.db
    }

    fn record(&self, at: &str, actor: &str, action: &str, target: &str, result: &str) {
        let mut entry = NewAuditRecord::ok(at, actor, "admin", "session", action, target);
        entry.result = result.to_string();
        self.db().record_audit(&entry).expect("record audit row");
    }

    async fn send(
        &self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        extra: &[(&str, String)],
    ) -> (StatusCode, HeaderMap, String) {
        send_to(
            api::router(self.state.clone()),
            method,
            uri,
            body,
            Some(TOKEN),
            extra,
        )
        .await
    }

    async fn get(&self, uri: &str) -> (StatusCode, HeaderMap, String) {
        self.send(Method::GET, uri, None, &[]).await
    }

    async fn get_json(&self, uri: &str) -> Value {
        let (status, _headers, body) = self.get(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        parse(&body)
    }
}

async fn send_to(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
    extra: &[(&str, String)],
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    for (name, value) in extra {
        builder = builder.header(*name, value.as_str());
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let request = builder.body(body).expect("valid request");
    let response = router.oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|err| panic!("invalid JSON ({err}): {body}"))
}

fn error_code(body: &str) -> String {
    parse(body)["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn actions(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["action"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn filters_paginate_and_keep_the_old_limit_behaviour() {
    let harness = Harness::new();
    harness.record(
        "2026-09-01T00:00:00Z",
        "alice@example.test",
        "instance.create",
        "alice",
        "ok",
    );
    harness.record(
        "2026-09-02T00:00:00Z",
        "alice@example.test",
        "instance.start",
        "alice",
        "ok",
    );
    harness.record(
        "2026-09-03T00:00:00Z",
        "bob@example.test",
        "instance.create",
        "bob",
        "failed",
    );
    harness.record(
        "2026-09-04T00:00:00Z",
        "bob@example.test",
        "token.revoke",
        "tok-1",
        "refused",
    );

    // The legacy `?limit=` call still returns the newest first.
    let entries = harness.get_json("/api/audit?limit=2").await;
    assert_eq!(actions(&entries), ["token.revoke", "instance.create"]);
    assert!(entries.as_array().expect("array")[0]
        .get("details")
        .is_none());

    let entries = harness
        .get_json("/api/audit?actor=alice@example.test")
        .await;
    assert_eq!(entries.as_array().expect("array").len(), 2);

    let entries = harness.get_json("/api/audit?action=instance.create").await;
    assert_eq!(entries.as_array().expect("array").len(), 2);

    let entries = harness.get_json("/api/audit?target=bob").await;
    assert_eq!(actions(&entries), ["instance.create"]);

    let entries = harness.get_json("/api/audit?result=failed").await;
    assert_eq!(actions(&entries), ["instance.create"]);

    let entries = harness
        .get_json("/api/audit?from=2026-09-02&to=2026-09-03")
        .await;
    assert_eq!(actions(&entries), ["instance.create", "instance.start"]);

    let entries = harness.get_json("/api/audit?limit=2&offset=2").await;
    assert_eq!(actions(&entries), ["instance.start", "instance.create"]);

    for invalid in [
        "/api/audit?limit=0",
        "/api/audit?limit=1001",
        "/api/audit?result=maybe",
        "/api/audit?from=yesterday",
        "/api/audit?from=2026-09-04&to=2026-09-01",
    ] {
        let (status, _headers, body) = harness.get(invalid).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}: {body}");
    }
}

#[tokio::test]
async fn a_mutation_carries_the_client_user_agent() {
    let harness = Harness::new();
    let (status, _headers, body) = harness
        .send(
            Method::POST,
            "/api/shares",
            Some(json!({"name": "findings"})),
            &[("user-agent", "fleett-probe/1.0".to_string())],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let entries = harness
        .get_json("/api/audit?action=share.create&result=ok")
        .await;
    let entry = entries.as_array().expect("array")[0].clone();
    assert_eq!(entry["user_agent"], "fleett-probe/1.0");
    assert_eq!(entry["result"], "ok");
}

#[tokio::test]
async fn export_is_signed_ndjson_and_verifiable_with_the_known_key() {
    let harness = Harness::with_audit_key();
    harness.record(
        "2026-09-01T00:00:00Z",
        "alice@example.test",
        "instance.create",
        "alice",
        "ok",
    );
    harness.record(
        "2026-09-02T00:00:00Z",
        "bob@example.test",
        "instance.start",
        "bob",
        "failed",
    );

    // A real mutation whose returned secret must never reach the trail.
    let (status, _headers, body) = harness
        .send(
            Method::POST,
            "/api/tokens",
            Some(json!({"name": "ci", "role": "viewer"})),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let issued = parse(&body)["token"].as_str().expect("token").to_string();
    assert!(issued.starts_with("flt_"));

    let (status, headers, body) = harness
        .get("/api/audit/export?from=2026-01-01&to=2100-01-01")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/x-ndjson")
    );
    let disposition = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .expect("Content-Disposition");
    assert!(
        disposition.starts_with("attachment; filename=\"fleett-audit-")
            && disposition.ends_with(".jsonl\""),
        "{disposition}"
    );
    let signature = headers
        .get("x-fleett-signature")
        .and_then(|value| value.to_str().ok())
        .expect("X-Fleett-Signature")
        .to_string();
    let expected = format!("sha256={}", fleett::audit::sign(AUDIT_KEY, body.as_bytes()));
    assert_eq!(
        signature, expected,
        "the signature must cover the exact body"
    );

    // One JSON object per line, newest first, `result` included.
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 3, "{body}");
    let newest = parse(lines[0]);
    assert_eq!(newest["action"], "token.create");
    assert_eq!(newest["result"], "ok");
    let oldest = parse(lines[2]);
    assert_eq!(oldest["action"], "instance.create");
    assert_eq!(oldest["at"], "2026-09-01T00:00:00Z");

    // Neither the issued token nor the bootstrap token may leak.
    assert!(!body.contains(&issued), "the export leaked a token secret");
    assert!(!body.contains(TOKEN), "the export leaked the bearer token");

    // The saved bytes verify with `fleett::audit::sign`.
    let recomputed = fleett::audit::sign(AUDIT_KEY, body.as_bytes());
    assert_eq!(
        signature.strip_prefix("sha256=").expect("prefix"),
        recomputed
    );
}

#[tokio::test]
async fn export_without_a_key_is_refused_with_instructions() {
    let harness = Harness::new();
    let (status, _headers, body) = harness
        .get("/api/audit/export?from=2026-01-01&to=2100-01-01")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error_code(&body), "audit_key_missing");
    let message = parse(&body)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(message.contains("FLEETT_AUDIT_HMAC_KEY"), "{message}");

    // Both bounds are mandatory: an unlabelled history is not evidence.
    let (status, _headers, body) = harness.get("/api/audit/export?from=2026-01-01").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "missing_range");
}

#[tokio::test]
async fn purge_deletes_expired_rows_and_leaves_an_audit_entry() {
    let harness = Harness::new();
    harness.record(
        "2000-01-01T00:00:00Z",
        "alice@example.test",
        "instance.create",
        "ancient",
        "ok",
    );
    let recent = fleett::models::now_iso();
    harness.record(
        &recent,
        "alice@example.test",
        "instance.start",
        "alice",
        "ok",
    );

    let (status, _headers, body) = harness
        .send(Method::DELETE, "/api/audit/expired", None, &[])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let report = parse(&body);
    assert_eq!(report["removed"], 1, "{report}");
    assert_eq!(report["retention"], "90d");
    assert!(report["cutoff"].as_str().is_some(), "{report}");

    let entries = harness.get_json("/api/audit?limit=100").await;
    let targets: Vec<&str> = entries
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|entry| entry["target"].as_str())
        .collect();
    assert!(!targets.contains(&"ancient"), "the old row must be gone");
    assert!(targets.contains(&"alice"), "the recent row must stay");
    let purges: Vec<&Value> = entries
        .as_array()
        .expect("array")
        .iter()
        .filter(|entry| entry["action"] == "audit.purge")
        .collect();
    assert_eq!(purges.len(), 1, "one purge must be journaled");
    assert_eq!(purges[0]["details"]["removed"], 1);
    assert_eq!(purges[0]["details"]["retention"], "90d");

    // The purge row is newer than the cutoff: a second run never eats it.
    let (status, _headers, body) = harness
        .send(Method::DELETE, "/api/audit/expired", None, &[])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["removed"], 0);

    // A direct DB query proves no row older than the cutoff survived.
    let cutoff = report["cutoff"].as_str().expect("cutoff");
    let remaining = harness
        .db()
        .query_audit(&AuditFilter {
            to: Some(cutoff.to_string()),
            limit: None,
            ..AuditFilter::default()
        })
        .expect("query");
    assert!(remaining.is_empty(), "{remaining:?}");
}
