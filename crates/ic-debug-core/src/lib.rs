//! Shared trace event types for ic-debug.
//!
//! Principals are stored as textual form (`Principal::to_text`) in events so
//! the schema is CBOR/JSON-portable without the Candid serde hooks.

use candid::CandidType;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type TraceId = Uuid;
pub type SpanId = u64;
pub type Seq = u64;

/// Propagated with every inter-canister call so the callee can attach its
/// events to the same causal trace.
#[derive(Clone, Copy, Debug, CandidType, Serialize, Deserialize)]
pub struct TraceHeader {
    /// UUID in its 16-byte raw form (Candid-friendlier than text).
    pub trace_id: [u8; 16],
    pub parent_seq: Seq,
    pub parent_span: SpanId,
}

impl TraceHeader {
    pub fn new(trace_id: TraceId, parent_seq: Seq, parent_span: SpanId) -> Self {
        Self { trace_id: *trace_id.as_bytes(), parent_seq, parent_span }
    }
    pub fn id(&self) -> TraceId {
        Uuid::from_bytes(self.trace_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub trace_id: TraceId,
    pub seq: Seq,
    pub parent_seq: Option<Seq>,
    pub span_id: SpanId,
    pub ts_nanos: u128,
    /// Canister principal in textual form, or `None` for agent-emitted events.
    pub canister: Option<String>,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    IngressEntered {
        method: String,
        caller: String,
        #[serde(with = "serde_bytes")]
        args_hash: Vec<u8>,
    },
    MethodEntered {
        method: String,
        caller: String,
        /// Rendered arguments, in declaration order, as `(name, value)` pairs.
        /// The value is `format!("{:?}", arg)` — the `#[trace_method]` macro
        /// captures these automatically so the timeline shows what the method
        /// was called with, without the canister author needing a manual
        /// `trace_event!` to echo its own args.
        #[serde(default)]
        args: Vec<(String, String)>,
    },
    MethodExited {
        reject: Option<String>,
    },
    CallSpawned {
        target: String,
        method: String,
        #[serde(with = "serde_bytes")]
        args_hash: Vec<u8>,
    },
    CallReturned {
        reject: Option<String>,
    },
    StateSnapshot {
        key: String,
        #[serde(with = "serde_bytes")]
        cbor: Vec<u8>,
    },
    TimerFired {
        label: String,
    },
    Note {
        label: String,
    },
}

/// CBOR serialize a watched field. Used by the `trace_state!` macro.
pub fn encode_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

// Re-export so the trace crate doesn't need its own ciborium dep.
pub use ciborium;
