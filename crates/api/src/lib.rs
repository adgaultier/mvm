//! HTTP API server exposing the sandbox manager over REST.

mod error;
mod routes;

use axum::Router;
use mvm_manager::Manager;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;

pub use error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub manager: Manager,
}

pub fn router(manager: Manager) -> Router {
    let state = AppState { manager };
    Router::new()
        .nest("/api/v1", routes::api_routes())
        .route("/health", axum::routing::get(|| async { "ok" }))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Serve the API on the given address until interrupted.
pub async fn serve(addr: SocketAddr, manager: Manager) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("mvm daemon listening on http://{addr}");
    axum::serve(listener, router(manager)).await
}
