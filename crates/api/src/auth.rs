//! Bearer-token authentication for the Agent API.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::{request::Parts, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mvm_common::api::ErrorResponse;
use mvm_common::Principal;

use crate::AppState;

/// Extractor resolving a request's bearer token to `Principal::Vm(vm_id)`.
///
/// Any failure (missing/malformed/unknown token) rejects with 401. The token
/// is deliberately never accepted on the control-plane router, which does not
/// mount this extractor.
pub struct VmPrincipal(pub Principal);

impl FromRequestParts<AppState> for VmPrincipal {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = match parts.headers.get(AUTHORIZATION) {
            Some(header) => match header.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")) {
                Some(token) if !token.is_empty() => token,
                _ => {
                    tracing::warn!("agent API unauthorized: malformed Authorization header");
                    return Err(unauthorized("expected 'Authorization: Bearer <token>'"));
                }
            },
            None => return Err(unauthorized("missing Authorization header")),
        };
        match state.manager.authenticate_vm(token) {
            Some(id) => Ok(Self(Principal::Vm(id))),
            None => {
                tracing::warn!("agent API unauthorized: invalid or expired token");
                Err(unauthorized("invalid or expired token"))
            }
        }
    }
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
        .into_response()
}
