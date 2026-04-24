//! The recorder daemon.
//!
//! Two intake paths:
//!   1. POST /events   — JSON array of decoded `Event`s from agent-js.
//!   2. POST /drain    — CBOR blob returned from a canister's `__debug_drain`
//!                       query; the daemon decodes it to `Vec<Event>`.
//!
//! Plus a tiny control surface:
//!   * POST /traces    — mint/register a trace (idempotent).
//!   * GET  /health    — liveness.

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use ic_debug_core::{Event, TraceId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::store::Store;

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
}

pub async fn run(store_path: &str, port: u16) -> Result<()> {
    let store = Arc::new(Store::open(store_path)?);
    let state = AppState { store };

    let app = Router::new()
        .route("/health", get(health))
        .route("/traces", post(register_trace))
        .route("/events", post(ingest_events))
        .route("/drain", post(ingest_drain))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    info!(%addr, store = %store_path, "recorder listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Deserialize)]
struct RegisterTrace {
    trace_id: TraceId,
    label: Option<String>,
}

#[derive(Debug, Serialize)]
struct OkCount {
    inserted: usize,
}

async fn register_trace(
    State(s): State<AppState>,
    Json(req): Json<RegisterTrace>,
) -> Result<impl IntoResponse, AppError> {
    s.store.upsert_trace(&req.trace_id.to_string(), req.label.as_deref())?;
    Ok(Json(OkCount { inserted: 1 }))
}

async fn ingest_events(
    State(s): State<AppState>,
    Json(events): Json<Vec<Event>>,
) -> Result<impl IntoResponse, AppError> {
    let mut inserted = 0;
    for ev in &events {
        s.store.upsert_trace(&ev.trace_id.to_string(), None)?;
        s.store.insert_event(ev)?;
        inserted += 1;
    }
    info!(count = inserted, "ingested events");
    Ok(Json(OkCount { inserted }))
}

/// POST /drain — body is the raw CBOR blob produced by a canister's
/// `__debug_drain` query (i.e. a CBOR-encoded `Vec<Event>`).
async fn ingest_drain(
    State(s): State<AppState>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, AppError> {
    let events: Vec<Event> = match ciborium::from_reader(body.as_ref()) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "drain blob: decode failed");
            return Err(AppError::BadRequest(format!("cbor decode: {e}")));
        }
    };
    let mut inserted = 0;
    for ev in &events {
        s.store.upsert_trace(&ev.trace_id.to_string(), None)?;
        s.store.insert_event(ev)?;
        inserted += 1;
    }
    info!(count = inserted, "ingested drain");
    Ok(Json(OkCount { inserted }))
}

// ---- error plumbing ----

enum AppError {
    BadRequest(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            AppError::Internal(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    }
}
