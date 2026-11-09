//! JSON request bodies with the shared [`ApiError`] envelope.
//!
//! [`axum::Json`] answers a malformed body with a plain-text rejection
//! (`Failed to deserialize the JSON body into the target type: …`), which tells
//! a client migrating from v0.4.0 nothing about the expected `env` schema.
//! [`ApiJson`] keeps the same success path and replaces every rejection with
//! the structured error body of this API.

use crate::api::ApiError;
use crate::models::{EnvInput, EnvShapeError};
use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::response::IntoResponse;
use serde::de::DeserializeOwned;
use serde_json::json;

/// Message returned whenever `env` has the wrong shape.
pub const INVALID_ENV_MESSAGE: &str = "env must be a list of {key,value,secret}";

/// `Json<T>` replacement: same success path, structured errors.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| {
                let status = rejection.into_response().status();
                ApiError::new(status, "invalid_body", "cannot read the request body")
            })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|err| {
            ApiError::bad_request(
                "invalid_body",
                format!("the request body is not valid JSON: {err}"),
            )
        })?;

        // `env` has a schema of its own. Check it before deserializing the
        // whole body so a wrong shape always yields `invalid_env` with both
        // accepted forms, never a generic type error.
        if let Some(env) = value.get("env") {
            if !env.is_null() {
                if let Err(err) = EnvInput::from_json_value(env) {
                    return Err(invalid_env(err));
                }
            }
        }

        serde_path_to_error::deserialize(value)
            .map(Self)
            .map_err(body_error)
    }
}

/// `400 invalid_env` naming the expected schema and both accepted shapes.
fn invalid_env(err: EnvShapeError) -> ApiError {
    ApiError::bad_request("invalid_env", INVALID_ENV_MESSAGE).with_details(json!({
        "expected": "sequence",
        "received": err.received,
        "example": [
            {"key": "DISCORD_ALLOWED_USERS", "value": "123,456", "secret": false},
            {"key": "DISCORD_BOT_TOKEN", "value": "…", "secret": true},
        ],
        "detail": err.detail,
    }))
}

/// `400 invalid_body` for any other deserialization failure, with the field
/// path when serde reports one.
fn body_error(err: serde_path_to_error::Error<serde_json::Error>) -> ApiError {
    let path = err.path().to_string();
    let inner = err.into_inner();
    let message = if path == "." {
        format!("the request body does not match the expected schema: {inner}")
    } else {
        format!("the request body field \"{path}\" is invalid: {inner}")
    };
    let mut details = json!({ "message": inner.to_string() });
    if path != "." {
        details["path"] = json!(path);
    }
    ApiError::bad_request("invalid_body", message).with_details(details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_env_shape_error_returns_the_expected_schema() {
        let err = EnvInput::from_json_value(&json!({"KEY": 1})).expect_err("bad map");
        let error = invalid_env(err);
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "invalid_env");
        assert_eq!(error.message, INVALID_ENV_MESSAGE);
        let details = error.details.expect("details");
        assert_eq!(details["expected"], "sequence");
        assert_eq!(details["received"], "map");
        assert!(details["example"].is_array());
    }

    #[test]
    fn body_errors_carry_the_field_path() {
        let err = serde_path_to_error::deserialize::<_, crate::models::PatchInstanceRequest>(
            json!({"mem_limit_mb": "big"}),
        )
        .expect_err("bad type");
        let error = body_error(err);
        assert_eq!(error.code, "invalid_body");
        let details = error.details.expect("details");
        assert_eq!(details["path"], "mem_limit_mb");
    }
}
