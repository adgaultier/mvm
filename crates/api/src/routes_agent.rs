//! Agent API routes (`/agent/v1`): the restricted, VM-authenticated surface a
//! sandbox can call back into the host. The caller's sandbox is derived from
//! the bearer token (`Principal::Vm(vm_id)`) — there is no `{id}` in the
//! paths, so a caller can only ever act on itself.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mvm_common::api::{DelegateRequest, ErrorResponse};
use mvm_common::Sandbox;

use crate::auth::VmPrincipal;
use crate::{ApiError, AppState};

pub fn agent_routes() -> Router<AppState> {
    Router::new()
        .route("/sandbox", get(get_self))
        .route("/sandbox/stop", post(stop_self))
        .route("/sandbox/delegate", post(delegate))
}

/// Inspect the caller's own sandbox.
async fn get_self(
    State(state): State<AppState>,
    principal: VmPrincipal,
) -> Result<Json<Sandbox>, ApiError> {
    let id = vm_id(&principal)?;
    Ok(Json(state.manager.get(id)?))
}

/// Stop the caller's own sandbox.
async fn stop_self(
    State(state): State<AppState>,
    principal: VmPrincipal,
) -> Result<Json<Sandbox>, ApiError> {
    let id = vm_id(&principal)?;
    Ok(Json(state.manager.stop(id).await?))
}

/// Launch a child clone of the caller's sandbox (same spec, but mounts and
/// command come from host policy), bounded by `timeout`. Authenticated and
/// authorized, but the delegation mechanics are not implemented yet.
async fn delegate(
    principal: VmPrincipal,
    Json(_req): Json<DelegateRequest>,
) -> Result<(StatusCode, Json<ErrorResponse>), ApiError> {
    vm_id(&principal)?;
    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse {
            error: "delegate is not yet implemented".into(),
        }),
    ))
}

fn vm_id(principal: &VmPrincipal) -> Result<&str, ApiError> {
    principal
        .0
        .vm_id()
        .map(|id| id.as_str())
        .ok_or_else(|| mvm_common::Error::Unauthorized("no VM identity".into()).into())
}
