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

/// TUI/`mvm ps` poll these list endpoints every couple of seconds; tracing
/// them would drown out real requests, so their span is disabled and the
/// access-log callbacks skip them (they only fire when the span is live).
fn is_poll_path(path: &str) -> bool {
    matches!(path, "/api/v1/images" | "/api/v1/sandboxes")
}

/// Apply per-request tracing to a router. The access log is emitted under the
/// `mvm::api` target (not tower-http's own), so `RUST_LOG=mvm::api=debug` turns
/// on "request received" / "response sent" lines. Poll paths are traced via
/// `Span::none()` — no span, no access-log lines for them.
fn with_trace_layer<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(
        TraceLayer::new_for_http()
            .make_span_with(|request: &axum::http::Request<axum::body::Body>| {
                if is_poll_path(request.uri().path()) {
                    tracing::Span::none()
                } else {
                    tracing::debug_span!(
                        "http-request",
                        method = %request.method(),
                        path = %request.uri().path()
                    )
                }
            })
            .on_request(
                |request: &axum::http::Request<axum::body::Body>, span: &tracing::Span| {
                    if !span.is_none() {
                        tracing::debug!(
                            target: "mvm::api",
                            method = %request.method(),
                            uri = %request.uri(),
                            "request received"
                        );
                    }
                },
            )
            .on_response(
                |response: &axum::http::Response<axum::body::Body>,
                 latency: std::time::Duration,
                 span: &tracing::Span| {
                    if !span.is_none() {
                        tracing::debug!(
                            target: "mvm::api",
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            "response sent"
                        );
                    }
                },
            )
            .on_failure(
                |class: tower_http::classify::ServerErrorsFailureClass,
                 latency: std::time::Duration,
                 _span: &tracing::Span| {
                    tracing::warn!(
                        target: "mvm::api",
                        class = %class,
                        latency_ms = latency.as_millis() as u64,
                        "request failed"
                    );
                },
            ),
    )
}

/// Control-plane router (currently unauthenticated; hardening is deferred).
pub fn router(manager: Manager) -> Router {
    let state = AppState { manager };
    with_trace_layer(
        Router::new()
            .nest("/api/v1", routes::api_routes())
            .route("/health", axum::routing::get(|| async { "ok" })),
    )
    .with_state(state)
}

/// Agent API router: every route requires a valid VM-scoped bearer token.
pub fn agent_router(manager: Manager) -> Router {
    let state = AppState { manager };
    with_trace_layer(Router::new().nest("/agent/v1", routes_agent::agent_routes()))
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
