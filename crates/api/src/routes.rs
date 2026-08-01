use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::stream::{self, StreamExt};
use mvm_common::api::{
    ExecRequest, InfoResponse, LogsQuery, PullRequest, ResizeRequest, StdinQuery,
};
use mvm_common::protocol::{encode_frame, AgentEvent};
use mvm_common::{ImageInfo, Sandbox, SandboxSpec};
use std::convert::Infallible;

use crate::{ApiError, AppState};

pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/info", get(info))
        .route("/sandboxes", get(list_sandboxes).post(create_sandbox))
        .route("/sandboxes/{id}", get(get_sandbox).delete(remove_sandbox))
        .route("/sandboxes/{id}/start", post(start_sandbox))
        .route("/sandboxes/{id}/stop", post(stop_sandbox))
        .route("/sandboxes/{id}/logs", get(logs))
        .route("/sandboxes/{id}/exec", post(exec))
        .route("/sandboxes/{id}/exec/{session}/stdin", post(exec_stdin))
        .route("/sandboxes/{id}/exec/{session}/resize", post(exec_resize))
        .route("/images", get(list_images))
        .route("/images/pull", post(pull_image))
        .route("/images/{*name}", delete(remove_image))
}

async fn info(State(_state): State<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        storage_driver: std::env::var("MVM_STORAGE_DRIVER")
            .unwrap_or_else(|_| "auto".into()),
    })
}

// ---- sandboxes -----------------------------------------------------------

async fn list_sandboxes(State(state): State<AppState>) -> Json<Vec<Sandbox>> {
    Json(state.manager.list())
}

async fn create_sandbox(
    State(state): State<AppState>,
    Json(spec): Json<SandboxSpec>,
) -> Result<(StatusCode, Json<Sandbox>), ApiError> {
    let sb = state.manager.create(spec)?;
    Ok((StatusCode::CREATED, Json(sb)))
}

async fn get_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Sandbox>, ApiError> {
    Ok(Json(state.manager.get(&id)?))
}

async fn remove_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.remove(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Sandbox>, ApiError> {
    Ok(Json(state.manager.start(&id).await?))
}

async fn stop_sandbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Sandbox>, ApiError> {
    Ok(Json(state.manager.stop(&id).await?))
}

// ---- logs ----------------------------------------------------------------

async fn logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Response, ApiError> {
    let (backlog, rx) = state.manager.logs(&id, q.follow)?;

    let backlog_stream = stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(backlog)) });

    let stream: stream::BoxStream<'static, Result<Bytes, Infallible>> = match rx {
        Some(rx) => {
            let live = stream::unfold(rx, |mut rx| async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match rx.recv().await {
                        Ok(bytes) => return Some((Ok(bytes), rx)),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return None,
                    }
                }
            });
            Box::pin(backlog_stream.chain(live))
        }
        None => Box::pin(backlog_stream),
    };

    Ok(Response::new(Body::from_stream(stream)))
}

// ---- exec ----------------------------------------------------------------

async fn exec(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Response, ApiError> {
    let (session, rx) = state
        .manager
        .exec(&id, req.argv, req.env, req.workdir, req.tty, req.cols, req.rows)
        .await?;

    // If the client goes away mid-session (Ctrl-C, crash, network drop),
    // dropping the response stream must not leave the guest process
    // running: the guard kills the session unless it completed normally.
    struct KillOnDrop {
        manager: mvm_manager::Manager,
        id: String,
        session: u32,
        armed: bool,
    }
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if self.armed {
                let _ = self.manager.exec_kill(&self.id, self.session);
            }
        }
    }
    let guard = KillOnDrop {
        manager: state.manager.clone(),
        id: id.clone(),
        session,
        armed: true,
    };

    // Stream framed AgentEvents; terminate after the Exit frame.
    let stream = stream::unfold(Some((rx, guard)), |state| async move {
        let (mut rx, mut guard) = state?;
        match rx.recv().await {
            Some(event) => {
                let done = matches!(event, AgentEvent::Exit { .. });
                if done {
                    guard.armed = false;
                }
                let frame = encode_frame(&event).unwrap_or_default();
                let next = if done { None } else { Some((rx, guard)) };
                Some((Ok::<Bytes, Infallible>(Bytes::from(frame)), next))
            }
            None => {
                // Agent gone (VM died): nothing left to kill.
                guard.armed = false;
                None
            }
        }
    });

    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        "x-mvm-exec-session",
        session.to_string().parse().expect("numeric header"),
    );
    Ok(resp)
}

/// Resize a live tty exec session.
async fn exec_resize(
    State(state): State<AppState>,
    Path((id, session)): Path<(String, u32)>,
    Json(req): Json<ResizeRequest>,
) -> Result<StatusCode, ApiError> {
    state.manager.exec_resize(&id, session, req.cols, req.rows)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Feed stdin into a live exec session. `?eof=true` closes stdin instead.
async fn exec_stdin(
    State(state): State<AppState>,
    Path((id, session)): Path<(String, u32)>,
    Query(q): Query<StdinQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let data = if q.eof { None } else { Some(body.to_vec()) };
    state.manager.exec_stdin(&id, session, data)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- images --------------------------------------------------------------

async fn list_images(State(state): State<AppState>) -> Result<Json<Vec<ImageInfo>>, ApiError> {
    Ok(Json(state.manager.images().list()?))
}

async fn remove_image(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.manager.images().remove(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pull_image(
    State(state): State<AppState>,
    Json(req): Json<PullRequest>,
) -> Response {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reference = req.reference.clone();
    let manager = state.manager.clone();

    tokio::task::spawn_blocking(move || {
        let result = manager.images().pull(&reference, |event| {
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = tx.send(line);
            }
        });
        match result {
            Ok(info) => {
                let _ = tx.send(
                    serde_json::json!({"stage": "pulled", "reference": info.reference, "digest": info.digest})
                        .to_string(),
                );
            }
            Err(e) => {
                let _ = tx.send(
                    serde_json::json!({"stage": "error", "error": e.to_string()}).to_string(),
                );
            }
        }
    });

    let stream = stream::poll_fn(move |cx| {
        match rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(line)) => {
                std::task::Poll::Ready(Some(Ok::<_, Infallible>(Bytes::from(format!("{line}\n")))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    });
    Response::new(Body::from_stream(stream))
}
