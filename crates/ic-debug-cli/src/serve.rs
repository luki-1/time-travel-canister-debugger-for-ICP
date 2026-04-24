//! `ic-debug serve` — HTTP read-only view over a recorded trace store.
//!
//! Exposes a small JSON API consumed by the React UI, plus static hosting
//! of the UI bundle (Vite build output at `ui/dist`).
//!
//!   GET /api/traces                      → list of registered traces
//!   GET /api/traces/:id                  → summary + causally-ordered events
//!   GET /api/traces/:id/diff             → state transitions (from diff engine)
//!   GET /api/traces/:id/snapshot/:can/:seq/:key → raw CBOR snapshot (hex-decoded)
//!   GET /api/canisters                   → principal → friendly name map
//!   PUT /api/canisters/:principal        → {"name": "..."} override
//!   DELETE /api/canisters/:principal     → drop a user override
//!   GET /health                          → liveness
//!
//! Anything that doesn't match `/api/*` or `/health` falls through to the
//! static UI so client-side routing can take over.
//!
//! Canister names come from two sources:
//!   1. Mapping files produced by `icp` (`.icp/cache/mappings/local.ids.json`
//!      and `.icp/data/mappings/ic.ids.json`) — loaded once at startup and
//!      treated as read-only defaults.
//!   2. User overrides in the `canister_names` table — editable from the UI.
//! The merge (override wins) is what the UI sees.

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, put},
    Json, Router,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::{info, warn};

use crate::{diff, replay};

#[derive(Clone)]
struct AppState {
    db_path: Arc<String>,
    /// Names loaded from icp mapping files at startup. Principal → name.
    /// Treated as read-only defaults; user overrides live in SQLite.
    default_names: Arc<BTreeMap<String, String>>,
}

pub async fn run(store_path: &str, port: u16, ui_dir: &str) -> Result<()> {
    // Fail fast if the store is unreachable, and apply the schema so the
    // `canister_names` table exists even if only the recorder has ever
    // touched this DB before.
    let setup = Connection::open(store_path)
        .with_context(|| format!("open sqlite store {store_path}"))?;
    setup
        .execute_batch(CANISTER_NAMES_SCHEMA)
        .context("ensure canister_names table")?;

    let default_names = load_default_names();
    if !default_names.is_empty() {
        info!(count = default_names.len(), "loaded canister name defaults");
    }

    let state = AppState {
        db_path: Arc::new(store_path.to_string()),
        default_names: Arc::new(default_names),
    };

    let api = Router::new()
        .route("/traces", get(list_traces))
        .route("/traces/:id", get(get_trace))
        .route("/traces/:id/diff", get(get_diff))
        .route("/traces/:id/snapshot/:canister/:seq/:key", get(get_snapshot))
        .route("/canisters", get(list_canisters))
        .route("/canisters/:principal", put(put_canister_name))
        .route("/canisters/:principal", delete(delete_canister_name))
        .with_state(state);

    let ui_path = PathBuf::from(ui_dir);
    let serve_dir = ServeDir::new(&ui_path).append_index_html_on_directories(true);

    let app = Router::new()
        .route("/health", get(health))
        .nest("/api", api)
        .fallback_service(serve_dir)
        .layer(CorsLayer::permissive());

    let addr = format!("127.0.0.1:{port}");
    info!(%addr, store = %store_path, ui = %ui_path.display(), "serve listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
struct TraceListRow {
    trace_id: String,
    started_at: i64,
    label: String,
    event_count: i64,
}

async fn list_traces(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    let mut stmt = conn.prepare(
        "SELECT t.id, t.started_at, t.label,
                (SELECT COUNT(*) FROM events e WHERE e.trace_id = t.id)
           FROM traces t
          WHERE t.label != ''
          ORDER BY t.started_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TraceListRow {
                trace_id: r.get(0)?,
                started_at: r.get(1)?,
                label: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                event_count: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Json(rows))
}

async fn get_trace(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    let (summary, events) = replay::load(&conn, &id)
        .map_err(|e| ApiError::Internal(format!("load trace: {e}")))?;
    if events.is_empty() {
        return Err(ApiError::NotFound(format!("trace {id} has no events")));
    }
    Ok(Json(serde_json::json!({
        "summary": summary,
        "events": events,
    })))
}

async fn get_diff(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    let (transitions, initials) = diff::walk_from_store(&conn, &id)
        .map_err(|e| ApiError::Internal(format!("diff walk: {e}")))?;
    Ok(Json(serde_json::json!({
        "trace_id": id,
        "transitions": transitions,
        "initials": initials,
    })))
}

async fn get_snapshot(
    State(s): State<AppState>,
    Path((id, canister, seq, key)): Path<(String, String, u64, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    let cbor: Vec<u8> = conn
        .query_row(
            "SELECT cbor FROM snapshots
              WHERE trace_id = ?1 AND canister = ?2 AND seq = ?3 AND key = ?4",
            rusqlite::params![id, canister, seq as i64, key],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::NotFound("snapshot not found".into())
            }
            other => ApiError::Internal(format!("query: {other}")),
        })?;

    let value: ciborium::Value = ciborium::from_reader(cbor.as_slice())
        .map_err(|e| ApiError::Internal(format!("decode cbor: {e}")))?;
    Ok(Json(serde_json::json!({
        "trace_id": id,
        "canister": canister,
        "seq": seq,
        "key": key,
        "bytes": cbor.len(),
        "display": diff::format_value(&value),
    })))
}

// ---- canister names ----

const CANISTER_NAMES_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS canister_names (
    principal TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
"#;

/// Shape the UI consumes: every principal we've seen through either the
/// mapping files OR a user override is listed with its effective name and
/// where that name came from, so the rename UI can show "(default)" hints.
#[derive(Debug, Serialize)]
struct CanisterNameRow {
    principal: String,
    name: String,
    source: &'static str, // "default" | "override"
    default_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RenameBody {
    name: String,
}

/// Walk likely mapping file locations and fold everything into one map.
/// Errors are logged and swallowed — names are optional polish, the UI
/// still works when nothing is found.
fn load_default_names() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let candidates = [
        ".icp/cache/mappings/local.ids.json",
        ".icp/data/mappings/ic.ids.json",
        ".icp/data/mappings/local.ids.json",
    ];
    for path in candidates {
        let p = StdPath::new(path);
        if !p.exists() {
            continue;
        }
        match std::fs::read_to_string(p) {
            Ok(text) => match serde_json::from_str::<BTreeMap<String, String>>(&text) {
                Ok(map) => {
                    for (name, principal) in map {
                        // mapping files are `name → principal`; invert for
                        // our principal-keyed lookup. First write wins so
                        // local.ids.json takes precedence over ic.ids.json
                        // when the same principal appears in both (unlikely
                        // but well-defined).
                        out.entry(principal).or_insert(name);
                    }
                }
                Err(e) => warn!(path = %p.display(), error = %e, "parse mapping file"),
            },
            Err(e) => warn!(path = %p.display(), error = %e, "read mapping file"),
        }
    }
    out
}

async fn list_canisters(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    let mut stmt = conn.prepare("SELECT principal, name FROM canister_names")?;
    let overrides: BTreeMap<String, String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut out: Vec<CanisterNameRow> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    // Overrides first — they take precedence and may exist for principals
    // not in the mapping files (e.g. the user pasted an arbitrary id).
    for (principal, name) in &overrides {
        let default_name = s.default_names.get(principal).cloned();
        out.push(CanisterNameRow {
            principal: principal.clone(),
            name: name.clone(),
            source: "override",
            default_name,
        });
        seen.insert(principal.clone());
    }
    for (principal, name) in s.default_names.as_ref() {
        if seen.contains(principal) {
            continue;
        }
        out.push(CanisterNameRow {
            principal: principal.clone(),
            name: name.clone(),
            source: "default",
            default_name: Some(name.clone()),
        });
    }
    Ok(Json(out))
}

async fn put_canister_name(
    State(s): State<AppState>,
    Path(principal): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<impl IntoResponse, ApiError> {
    let trimmed = body.name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let conn = Connection::open(s.db_path.as_str())?;
    conn.execute(
        "INSERT INTO canister_names (principal, name) VALUES (?1, ?2)
         ON CONFLICT(principal) DO UPDATE SET name = excluded.name",
        params![principal, trimmed],
    )?;
    Ok(Json(serde_json::json!({
        "principal": principal,
        "name": trimmed,
        "source": "override",
        "default_name": s.default_names.get(&principal),
    })))
}

async fn delete_canister_name(
    State(s): State<AppState>,
    Path(principal): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let conn = Connection::open(s.db_path.as_str())?;
    conn.execute(
        "DELETE FROM canister_names WHERE principal = ?1",
        params![principal],
    )?;
    // Report the effective state after the delete so the UI can update
    // without refetching — it's either the default or nothing.
    let default_name = s.default_names.get(&principal).cloned();
    Ok(Json(serde_json::json!({
        "principal": principal,
        "name": default_name,
        "source": if default_name.is_some() { "default" } else { "none" },
    })))
}

// ---- error plumbing ----

enum ApiError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::Internal(format!("sqlite: {e}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
        }
    }
}
