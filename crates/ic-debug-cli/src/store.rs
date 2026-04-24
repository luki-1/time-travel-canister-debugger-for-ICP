use anyhow::{Context, Result};
use ic_debug_core::{Event, EventKind};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).context("open sqlite")?;
        conn.execute_batch(SCHEMA).context("apply schema")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn upsert_trace(&self, trace_id: &str, label: Option<&str>) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO traces (id, started_at, label) VALUES (?1, ?2, ?3)",
            params![
                trace_id,
                chrono_now_millis(),
                label.unwrap_or("")
            ],
        )?;
        Ok(())
    }

    pub fn insert_event(&self, ev: &Event) -> Result<()> {
        let c = self.conn.lock().unwrap();
        let trace_id = ev.trace_id.to_string();
        let (kind, method, caller, target, reject, snapshot_key) = decompose(&ev.kind);
        let payload = serde_json::to_string(&ev.kind)?;

        let canister = ev.canister.clone().unwrap_or_default();
        c.execute(
            "INSERT OR REPLACE INTO events
             (trace_id, canister, seq, parent_seq, span_id, ts_nanos, kind,
              method, caller, target, reject, snapshot_key, payload_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                trace_id,
                canister,
                ev.seq as i64,
                ev.parent_seq.map(|v| v as i64),
                ev.span_id as i64,
                ev.ts_nanos.to_string(), // u128 → text to avoid rusqlite i64 limit
                kind,
                method,
                caller,
                target,
                reject,
                snapshot_key,
                payload,
            ],
        )?;

        if let EventKind::StateSnapshot { key, cbor } = &ev.kind {
            c.execute(
                "INSERT OR REPLACE INTO snapshots (trace_id, canister, seq, key, cbor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![trace_id, canister, ev.seq as i64, key, cbor],
            )?;
        }
        Ok(())
    }

    pub fn trace_count(&self) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM traces", [], |r| r.get(0))?;
        Ok(n)
    }

    pub fn event_count(&self, trace_id: &str) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        let n: i64 = c.query_row(
            "SELECT COUNT(*) FROM events WHERE trace_id = ?1",
            params![trace_id],
            |r| r.get(0),
        )?;
        Ok(n)
    }
}

fn decompose(
    k: &EventKind,
) -> (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match k {
        EventKind::IngressEntered { method, caller, .. } => (
            "ingress_entered",
            Some(method.clone()),
            Some(caller.clone()),
            None,
            None,
            None,
        ),
        EventKind::MethodEntered { method, caller, .. } => (
            "method_entered",
            Some(method.clone()),
            Some(caller.clone()),
            None,
            None,
            None,
        ),
        EventKind::MethodExited { reject } => {
            ("method_exited", None, None, None, reject.clone(), None)
        }
        EventKind::CallSpawned { target, method, .. } => (
            "call_spawned",
            Some(method.clone()),
            None,
            Some(target.clone()),
            None,
            None,
        ),
        EventKind::CallReturned { reject } => {
            ("call_returned", None, None, None, reject.clone(), None)
        }
        EventKind::StateSnapshot { key, .. } => {
            ("state_snapshot", None, None, None, None, Some(key.clone()))
        }
        EventKind::TimerFired { label } => ("timer_fired", Some(label.clone()), None, None, None, None),
        EventKind::Note { label } => ("note", Some(label.clone()), None, None, None, None),
    }
}

fn chrono_now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS traces (
    id TEXT PRIMARY KEY,
    started_at INTEGER NOT NULL,
    label TEXT
);
CREATE TABLE IF NOT EXISTS events (
    trace_id TEXT NOT NULL,
    canister TEXT NOT NULL,
    seq INTEGER NOT NULL,
    parent_seq INTEGER,
    span_id INTEGER NOT NULL,
    ts_nanos TEXT NOT NULL,
    kind TEXT NOT NULL,
    method TEXT,
    caller TEXT,
    target TEXT,
    reject TEXT,
    snapshot_key TEXT,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (trace_id, canister, seq)
);
CREATE INDEX IF NOT EXISTS idx_events_trace ON events(trace_id, ts_nanos);
CREATE TABLE IF NOT EXISTS snapshots (
    trace_id TEXT NOT NULL,
    canister TEXT NOT NULL,
    seq INTEGER NOT NULL,
    key TEXT NOT NULL,
    cbor BLOB NOT NULL,
    PRIMARY KEY (trace_id, canister, seq, key)
);
-- User-supplied labels for canister principals, editable from the UI.
-- Overrides any name seeded from mapping files at serve startup.
CREATE TABLE IF NOT EXISTS canister_names (
    principal TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
"#;
