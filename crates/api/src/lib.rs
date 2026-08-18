//! HTTP API server exposing the sandbox manager over REST.
//!
//! `/api/v1` (`router`) is the privileged control plane, used by the
//! CLI/TUI. The Agent API — the restricted surface a sandbox calls back into
//! the host with — no longer rides HTTP: it is a per-sandbox vsock channel
//! set up by `mvm-manager` when the guest boots (see
//! `mvm_common::protocol::AGENT_API_VSOCK_PORT`), not exposed by this
//! listener at all.

mod error;
mod routes;

use axum::Router;
use mvm_manager::Manager;
use std::net::{Ipv4Addr, SocketAddr};
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

/// Serve the control plane until interrupted. Always binds loopback — the
/// control plane is unauthenticated, so `port` is the only configurable
/// part of the listen address.
pub async fn serve(port: u16, manager: Manager) -> std::io::Result<()> {
    let control_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let control_listener = tokio::net::TcpListener::bind(control_addr).await?;
    tracing::info!("mvm daemon listening on http://{control_addr}");
    axum::serve(control_listener, router(manager)).await
}
