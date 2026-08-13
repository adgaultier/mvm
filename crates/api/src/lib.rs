//! HTTP API server exposing the sandbox manager over REST.
//!
//! Two surfaces, bound to different listeners:
//! - the privileged control plane (`/api/v1`, `router`), used by the CLI/TUI;
//! - the restricted Agent API (`/agent/v1`, `agent_router`), authenticated by
//!   each VM's scoped bearer token and authorized against the caller's own
//!   sandbox only.

mod auth;
mod error;
mod routes;
mod routes_agent;

use axum::Router;
use mvm_manager::Manager;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

pub use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub manager: Manager,
}

/// Control-plane router (currently unauthenticated; hardening is deferred).
pub fn router(manager: Manager) -> Router {
    let state = AppState { manager };
    Router::new()
        .nest("/api/v1", routes::api_routes())
        .route("/health", axum::routing::get(|| async { "ok" }))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Agent API router: every route requires a valid VM-scoped bearer token.
pub fn agent_router(manager: Manager) -> Router {
    let state = AppState { manager };
    Router::new()
        .nest("/agent/v1", routes_agent::agent_routes())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Serve the control plane and the Agent API on their own listeners until one
/// of them stops.
pub async fn serve(
    control_addr: SocketAddr,
    agent_addr: SocketAddr,
    manager: Manager,
) -> std::io::Result<()> {
    if control_addr == agent_addr {
        return Err(std::io::Error::other(
            "control-plane and agent listeners must use different addresses",
        ));
    }
    let control_listener = tokio::net::TcpListener::bind(control_addr).await?;
    let agent_listener = tokio::net::TcpListener::bind(agent_addr).await?;
    tracing::info!("mvm daemon listening on http://{control_addr}");
    tracing::info!("mvm agent API listening on http://{agent_addr}");
    let control = axum::serve(control_listener, router(manager.clone()));
    let agent = axum::serve(agent_listener, agent_router(manager));
    tokio::select! {
        r = control => r,
        r = agent => r,
    }
}
