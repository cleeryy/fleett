//! Per-instance authorization (issue #37).
//!
//! Authentication resolves a request to an [`AuthContext`]; this module decides
//! what that identity may do on one instance. Three levels exist:
//!
//! * `owner` — read, write and exec, plus managing the instance ACL;
//! * `operator` — read and write;
//! * `viewer` — read only.
//!
//! The level comes from, in order: the global `admin` role, an explicit
//! `instance_acl` row for the caller's subject, or membership in a scoped
//! token's `instances` list (which grants at most `viewer`). A bearer token
//! created before scopes existed keeps the legacy global behaviour.
//!
//! [`require_instance_access`] applies the rules centrally for every
//! `/api/instances/{id}/...` route; the handlers only add the checks the path
//! cannot express (ACL management, instance creation).

use crate::api::error::ApiError;
use crate::api::AppState;
pub use crate::auth::Action;
use crate::auth::{AuthContext, AuthMethod, Role};
use crate::db::AclRecord;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Level granted on one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceRole {
    Owner,
    Operator,
    Viewer,
}

impl InstanceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "owner" => Some(Self::Owner),
            "operator" => Some(Self::Operator),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }

    /// Whether this level allows `action`.
    pub fn permits(self, action: Action) -> bool {
        match action {
            Action::Read => true,
            Action::Write => matches!(self, Self::Owner | Self::Operator),
            Action::Exec => matches!(self, Self::Owner),
        }
    }
}

impl std::fmt::Display for InstanceRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `instance_acl` row, with its role already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclEntry {
    pub instance_id: String,
    pub subject: String,
    pub role: InstanceRole,
}

/// Convert stored rows to validated entries; a row with an unknown role grants
/// nothing and is dropped.
pub fn entries(rows: Vec<AclRecord>) -> Vec<AclEntry> {
    rows.into_iter()
        .filter_map(|row| {
            InstanceRole::parse(&row.role).map(|role| AclEntry {
                instance_id: row.instance_id,
                subject: row.subject,
                role,
            })
        })
        .collect()
}

/// Effective level of `context` on `instance`.
///
/// `None` means the caller may not even read the instance. The ACL slice is
/// expected to hold the rows relevant to this request (at least those of the
/// caller's subject).
pub fn instance_role(
    context: &AuthContext,
    acl: &[AclEntry],
    instance: &str,
) -> Option<InstanceRole> {
    if context.role == Role::Admin {
        return Some(InstanceRole::Owner);
    }
    if let Some(entry) = acl
        .iter()
        .find(|entry| entry.instance_id == instance && entry.subject == context.subject)
    {
        return Some(entry.role);
    }
    match &context.scope {
        // A scope lists the instances the token may address; it grants at most
        // `viewer`, the actions still gate what the token can do.
        Some(scope) => scope
            .allows_instance(instance)
            .then_some(InstanceRole::Viewer),
        // Legacy bearer tokens (scope NULL) keep the global role-based access
        // they had before this feature. A session viewer, on the other hand,
        // only sees the instances an ACL gives them.
        None if context.method == AuthMethod::Bearer => Some(InstanceRole::Viewer),
        None => None,
    }
}

/// Whether `context` may perform `action` on `instance`.
///
/// A token scope always intersects: an admin token that carries an explicit
/// scope only reaches the instances and actions that scope lists.
pub fn can(context: &AuthContext, acl: &[AclEntry], instance: &str, action: Action) -> bool {
    if let Some(scope) = &context.scope {
        if !scope.allows_instance(instance) || !scope.allows_action(action) {
            return false;
        }
    }
    match instance_role(context, acl, instance) {
        Some(role) => role.permits(action),
        None => false,
    }
}

/// Whether `context` is an `owner` of `instance` (an admin always is).
pub fn is_owner(context: &AuthContext, acl: &[AclEntry], instance: &str) -> bool {
    instance_role(context, acl, instance) == Some(InstanceRole::Owner)
}

/// Filter `all_ids` down to the instances `context` may read, in input order.
pub fn visible_instances(
    context: &AuthContext,
    acl: &[AclEntry],
    all_ids: &[String],
) -> Vec<String> {
    all_ids
        .iter()
        .filter(|id| can(context, acl, id, Action::Read))
        .cloned()
        .collect()
}

/// Percent-decode one path segment.
///
/// The middleware sees the raw URI while axum's `Path` extractor decodes it,
/// so `/instances/%61lice/files` would otherwise name `alice` without any
/// authorization check. Invalid escapes are kept literally; the instance then
/// fails to resolve and the handler answers `404`.
fn percent_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `(instance id, action)` addressed by one request path, if any.
///
/// The middleware runs on the router nested at `/api`, so the path usually
/// arrives as `/instances/{id}/...`; the optional prefix keeps the helper
/// usable on the full URI too. `/instances/import` is a create, not an
/// instance id, and returns `None` (the handler requires an admin).
pub fn route_target(path: &str, method: &Method) -> Option<(String, Action)> {
    let path = path.strip_prefix("/api").unwrap_or(path);
    let mut segments = path.trim_start_matches('/').split('/');
    if segments.next()? != "instances" {
        return None;
    }
    let id = percent_decode(segments.next()?);
    if id.is_empty() || id == "import" {
        return None;
    }
    let rest: Vec<String> = segments.map(percent_decode).collect();
    let action = if rest.iter().any(|segment| segment == "exec") {
        Action::Exec
    } else if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        Action::Read
    } else {
        Action::Write
    };
    Some((id, action))
}

/// Middleware enforcing [`can`] before the handler runs.
///
/// An unknown instance is left to the handler, so the response stays `404` for
/// everybody (a `403` would confirm that the id exists). A known instance the
/// caller may not address answers `403 forbidden`.
pub async fn require_instance_access(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(context) = request.extensions().get::<AuthContext>().cloned() else {
        // `require_auth` runs first and already answered; be defensive.
        return Ok(next.run(request).await);
    };
    let Some((instance, action)) = route_target(request.uri().path(), request.method()) else {
        return Ok(next.run(request).await);
    };
    if context.role == Role::Admin && context.scope.is_none() {
        return Ok(next.run(request).await);
    }
    if !state.db.exists(&instance).map_err(ApiError::from)? {
        return Ok(next.run(request).await);
    }
    let acl = state
        .db
        .acl_for_subject(&context.subject)
        .map(entries)
        .map_err(ApiError::from)?;
    if can(&context, &acl, &instance, action) {
        return Ok(next.run(request).await);
    }
    Err(ApiError::forbidden(
        "forbidden",
        format!(
            "{} access to instance \"{instance}\" is not granted",
            action
        ),
    )
    .with_details(json!({
        "instance": instance,
        "action": action.as_str(),
        "subject": context.subject,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenScope;

    fn context(role: Role, method: AuthMethod, subject: &str) -> AuthContext {
        AuthContext {
            actor: subject.to_string(),
            subject: subject.to_string(),
            role,
            method,
            user: None,
            scope: None,
            ip: None,
        }
    }

    fn acl(instance: &str, subject: &str, role: InstanceRole) -> AclEntry {
        AclEntry {
            instance_id: instance.to_string(),
            subject: subject.to_string(),
            role,
        }
    }

    #[test]
    fn instance_roles_round_trip_and_permit_their_actions() {
        assert_eq!(InstanceRole::parse("OWNER"), Some(InstanceRole::Owner));
        assert_eq!(
            InstanceRole::parse("operator"),
            Some(InstanceRole::Operator)
        );
        assert_eq!(InstanceRole::parse("viewer"), Some(InstanceRole::Viewer));
        assert_eq!(InstanceRole::parse("admin"), None);
        assert_eq!(InstanceRole::Operator.as_str(), "operator");

        assert!(InstanceRole::Viewer.permits(Action::Read));
        assert!(!InstanceRole::Viewer.permits(Action::Write));
        assert!(InstanceRole::Operator.permits(Action::Write));
        assert!(!InstanceRole::Operator.permits(Action::Exec));
        assert!(InstanceRole::Owner.permits(Action::Exec));
    }

    #[test]
    fn an_admin_is_an_owner_whatever_the_acl_says() {
        let admin = context(Role::Admin, AuthMethod::Session, "root@example.test");
        assert_eq!(
            instance_role(&admin, &[], "probe"),
            Some(InstanceRole::Owner)
        );
        assert!(can(&admin, &[], "probe", Action::Exec));
    }

    #[test]
    fn an_explicit_acl_row_wins_over_the_legacy_default() {
        let bearer = context(Role::Viewer, AuthMethod::Bearer, "ci");
        let rows = [acl("probe", "ci", InstanceRole::Operator)];
        assert_eq!(
            instance_role(&bearer, &rows, "probe"),
            Some(InstanceRole::Operator)
        );
        assert!(can(&bearer, &rows, "probe", Action::Write));
        assert!(!can(&bearer, &rows, "probe", Action::Exec));
        // Another instance is not covered by the row.
        assert_eq!(
            instance_role(&bearer, &rows, "other"),
            Some(InstanceRole::Viewer)
        );
    }

    #[test]
    fn a_viewer_session_only_sees_instances_it_has_an_acl_on() {
        let viewer = context(Role::Viewer, AuthMethod::Session, "user@example.test");
        assert_eq!(instance_role(&viewer, &[], "probe"), None);
        assert!(!can(&viewer, &[], "probe", Action::Read));
        let rows = [acl("probe", "user@example.test", InstanceRole::Viewer)];
        assert!(can(&viewer, &rows, "probe", Action::Read));
        assert!(!can(&viewer, &rows, "probe", Action::Write));
    }

    #[test]
    fn a_legacy_bearer_token_keeps_the_global_viewer_read() {
        let bearer = context(Role::Viewer, AuthMethod::Bearer, "legacy");
        assert!(can(&bearer, &[], "anything", Action::Read));
        assert!(!can(&bearer, &[], "anything", Action::Write));
    }

    #[test]
    fn a_token_scope_restricts_instances_and_actions() {
        let mut token = context(Role::Admin, AuthMethod::Bearer, "scoped");
        token.scope = Some(TokenScope {
            instances: vec!["probe".into()],
            actions: vec![Action::Read],
        });
        assert!(can(&token, &[], "probe", Action::Read));
        assert!(!can(&token, &[], "probe", Action::Write));
        assert!(!can(&token, &[], "probe", Action::Exec));
        assert!(!can(&token, &[], "tchouniz", Action::Read));
    }

    #[test]
    fn the_visible_list_is_filtered_in_input_order() {
        let viewer = context(Role::Viewer, AuthMethod::Session, "user@example.test");
        let rows = [
            acl("probe", "user@example.test", InstanceRole::Viewer),
            acl("tchouniz", "someone-else", InstanceRole::Viewer),
        ];
        let all = vec!["probe".to_string(), "tchouniz".to_string()];
        assert_eq!(visible_instances(&viewer, &rows, &all), vec!["probe"]);

        let admin = context(Role::Admin, AuthMethod::Bearer, "bootstrap");
        assert_eq!(visible_instances(&admin, &[], &all), all);
    }

    #[test]
    fn route_targets_are_classified_by_method_and_path() {
        assert_eq!(
            route_target("/instances/probe/logs", &Method::GET),
            Some(("probe".to_string(), Action::Read))
        );
        assert_eq!(
            route_target("/api/instances/probe/files/content", &Method::PUT),
            Some(("probe".to_string(), Action::Write))
        );
        assert_eq!(
            route_target("/instances/probe", &Method::DELETE),
            Some(("probe".to_string(), Action::Write))
        );
        assert_eq!(
            route_target("/instances/probe/exec", &Method::POST),
            Some(("probe".to_string(), Action::Exec))
        );
        assert_eq!(route_target("/instances", &Method::GET), None);
        assert_eq!(route_target("/instances/import", &Method::POST), None);
        assert_eq!(route_target("/tokens", &Method::POST), None);
        assert_eq!(route_target("/messages", &Method::GET), None);
        // A percent-encoded id must be resolved *before* the ACL check,
        // otherwise `/instances/%61lice` would bypass the middleware.
        assert_eq!(
            route_target("/instances/%61lice/files", &Method::GET),
            Some(("alice".to_string(), Action::Read))
        );
        assert_eq!(
            route_target("/instances/probe/%65xec", &Method::POST),
            Some(("probe".to_string(), Action::Exec))
        );
    }

    #[test]
    fn invalid_acl_rows_grant_nothing() {
        let rows = vec![
            AclRecord {
                instance_id: "probe".into(),
                subject: "user@example.test".into(),
                role: "root".into(),
                created_at: "2026-11-09T00:00:00Z".into(),
            },
            AclRecord {
                instance_id: "probe".into(),
                subject: "user@example.test".into(),
                role: "viewer".into(),
                created_at: "2026-11-09T00:00:00Z".into(),
            },
        ];
        let entries = entries(rows);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, InstanceRole::Viewer);
    }
}
