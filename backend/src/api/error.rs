//! Error type shared by every route.

use crate::db::DbError;
use crate::driver::DriverError;
use crate::render::RenderError;
use crate::secrets::{SecretsError, MISSING_KEY_CODE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Compose stderr is truncated to this many characters before it is returned.
pub const MAX_DRIVER_DETAIL_CHARS: usize = 2048;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Optional machine-readable payload, returned as `error.details`.
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    /// Attach a structured payload (returned as `error.details`).
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required: send a bearer token or a valid session cookie",
        )
    }

    pub fn unauthorized_with(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    /// 502 with the compose stderr, truncated to 2 KB.
    pub fn driver(err: DriverError) -> Self {
        let detail = truncate_chars(err.detail.trim(), MAX_DRIVER_DETAIL_CHARS);
        Self::new(
            StatusCode::BAD_GATEWAY,
            "driver_error",
            format!("docker compose {} failed: {detail}", err.operation),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = serde_json::Map::new();
        error.insert("code".to_string(), json!(self.code));
        error.insert("message".to_string(), json!(self.message));
        if let Some(details) = self.details {
            error.insert("details".to_string(), details);
        }
        let body = json!({ "error": error });
        (self.status, Json(body)).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Conflict(id) => {
                ApiError::conflict("already_exists", format!("{id} already exists"))
            }
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<RenderError> for ApiError {
    fn from(err: RenderError) -> Self {
        match err {
            RenderError::UnknownTemplate(id) => {
                ApiError::bad_request("unknown_template", format!("unknown template \"{id}\""))
            }
            RenderError::InvalidYaml(detail) => ApiError::unprocessable(
                "invalid_template",
                format!("the template does not render to valid Hermes YAML: {detail}"),
            ),
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<SecretsError> for ApiError {
    fn from(err: SecretsError) -> Self {
        match err {
            SecretsError::MissingKey => {
                ApiError::conflict(MISSING_KEY_CODE, SecretsError::MissingKey.to_string())
            }
            other => ApiError::internal(other.to_string()),
        }
    }
}

/// Truncate on a character boundary (compose stderr may contain UTF-8).
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_details() {
        let short = truncate_chars("boom", 10);
        assert_eq!(short, "boom");

        let long = "é".repeat(50);
        let truncated = truncate_chars(&long, 10);
        assert_eq!(truncated.chars().count(), 11);
        assert!(truncated.ends_with('…'));
    }
}
