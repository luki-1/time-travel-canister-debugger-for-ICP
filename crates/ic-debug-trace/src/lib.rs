//! Canister-side instrumentation for ic-debug.
//!
//! Exposed surface:
//!   * `#[trace_method]` — attribute macro from `ic-debug-trace-macros`.
//!   * `trace_event!` / `trace_state!` — manual markers.
//!   * `call_traced(target, method, args, header)` — propagates the trace
//!     header through an inter-canister call and logs spawn/return.
//!   * `begin_trace(header)` — adopt the incoming header at method entry.
//!
//! When the `enabled` feature is off every runtime hook short-circuits so
//! production builds pay essentially zero overhead.

pub use ic_debug_core as core;
pub use ic_debug_trace_macros::trace_method;

use core::{Event, EventKind, Seq, SpanId, TraceHeader, TraceId};
use sha2::{Digest, Sha256};
use std::cell::RefCell;

thread_local! {
    static SINK: RefCell<RingBuffer> = RefCell::new(RingBuffer::with_capacity(4096));
    static STATE: RefCell<TraceState> = RefCell::new(TraceState::default());
}

#[derive(Default)]
struct TraceState {
    current_trace: Option<TraceId>,
    current_span: SpanId,
    next_seq: Seq,
    next_span: SpanId,
    last_seq: Option<Seq>,
}

struct RingBuffer {
    cap: usize,
    buf: Vec<Event>,
}

impl RingBuffer {
    fn with_capacity(cap: usize) -> Self {
        Self { cap, buf: Vec::with_capacity(cap) }
    }
    fn push(&mut self, e: Event) {
        if self.buf.len() >= self.cap {
            self.buf.remove(0);
        }
        self.buf.push(e);
    }
    fn drain(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.buf)
    }
}

/// Adopt an incoming trace header. Called at the top of every wrapped
/// method that carries a `TraceHeader` as first arg.
pub fn begin_trace(header: TraceHeader) -> SpanId {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.current_trace = Some(header.id());
        s.next_span += 1;
        s.current_span = s.next_span;
        s.last_seq = Some(header.parent_seq);
        s.current_span
    })
}

pub fn current_header() -> Option<TraceHeader> {
    STATE.with(|s| {
        let s = s.borrow();
        s.current_trace.map(|id| TraceHeader {
            trace_id: *id.as_bytes(),
            parent_seq: s.last_seq.unwrap_or(0),
            parent_span: s.current_span,
        })
    })
}

// -------- Event recording --------

pub fn record(kind: EventKind) {
    #[cfg(not(feature = "enabled"))]
    {
        let _ = kind;
    }
    #[cfg(feature = "enabled")]
    {
        let (trace_id, span_id, seq, parent_seq) = STATE.with(|s| {
            let mut s = s.borrow_mut();
            let seq = s.next_seq;
            s.next_seq += 1;
            let parent_seq = s.last_seq;
            s.last_seq = Some(seq);
            (s.current_trace, s.current_span, seq, parent_seq)
        });
        let Some(trace_id) = trace_id else { return };
        let ev = Event {
            trace_id,
            seq,
            parent_seq,
            span_id,
            ts_nanos: now_nanos(),
            canister: Some(ic_cdk::api::id().to_text()),
            kind,
        };
        SINK.with(|s| s.borrow_mut().push(ev));
    }
}

pub fn drain() -> Vec<u8> {
    let events = SINK.with(|s| s.borrow_mut().drain());
    let mut buf = Vec::new();
    let _ = ciborium::into_writer(&events, &mut buf);
    buf
}

// -------- Method-entry hooks used by #[trace_method] --------

pub fn on_method_enter(method: &'static str, args: Vec<(String, String)>) {
    #[cfg(target_arch = "wasm32")]
    let caller = ic_cdk::api::caller().to_text();
    #[cfg(not(target_arch = "wasm32"))]
    let caller = candid::Principal::anonymous().to_text();
    record(EventKind::MethodEntered { method: method.to_string(), caller, args });
}

pub struct MethodExitGuard;
impl Drop for MethodExitGuard {
    fn drop(&mut self) {
        record(EventKind::MethodExited { reject: None });
    }
}

// -------- call_traced --------

/// Macro form that expands to a plain `ic_cdk::call` with the trace header
/// spliced in as the first positional argument, and emits spawn/return
/// events around the await.
///
/// The callee's method signature must accept `(TraceHeader, ...OrigArgs)`.
#[macro_export]
macro_rules! call_traced {
    ($target:expr, $method:expr, ( $( $arg:expr ),* $(,)? )) => {{
        let __target: ::candid::Principal = $target;
        let __method: &str = $method;
        // Hash the original args (without header) so record() below has
        // something sensible to emit even though the final on-the-wire
        // payload will prepend the freshly-stamped TraceHeader.
        let __args_encoded = ::candid::encode_args(( $( $arg, )* ))
            .expect("call_traced: encode args (hash)");
        let __hash = $crate::sha256(&__args_encoded).to_vec();
        // Record CallSpawned FIRST so last_seq advances to this event's
        // seq; then the header we build below carries that seq as
        // parent_seq, giving the callee a direct causal back-edge.
        $crate::record($crate::core::EventKind::CallSpawned {
            target: __target.to_text(),
            method: __method.to_string(),
            args_hash: __hash,
        });
        let __header = $crate::current_header().unwrap_or_else(|| {
            $crate::core::TraceHeader {
                trace_id: *$crate::uuid::Uuid::nil().as_bytes(),
                parent_seq: 0,
                parent_span: 0,
            }
        });
        let __encoded = ::candid::encode_args((__header, $( $arg ),*))
            .expect("call_traced: encode args (wire)");
        let __reply = ::ic_cdk::api::call::call_raw(__target, __method, &__encoded, 0).await;
        match __reply {
            Ok(__bytes) => {
                $crate::record($crate::core::EventKind::CallReturned { reject: None });
                ::candid::decode_args::<_>(&__bytes)
                    .map_err(|e| (
                        ::ic_cdk::api::call::RejectionCode::CanisterError,
                        format!("decode: {e}"),
                    ))
            }
            Err((__code, __msg)) => {
                $crate::record($crate::core::EventKind::CallReturned { reject: Some(__msg.clone()) });
                Err((__code, __msg))
            }
        }
    }};
}

#[doc(hidden)]
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

// re-export uuid so the call_traced! macro can reach it from user crates.
pub use uuid;

#[cfg(target_arch = "wasm32")]
fn now_nanos() -> u128 {
    ic_cdk::api::time() as u128
}

#[cfg(not(target_arch = "wasm32"))]
fn now_nanos() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

// -------- Manual trace helpers --------

#[macro_export]
macro_rules! trace_event {
    ($label:expr) => {{
        $crate::record($crate::core::EventKind::Note {
            label: ($label).to_string(),
        });
    }};
}

#[macro_export]
macro_rules! trace_state {
    ($key:expr, $value:expr) => {{
        if let Ok(cbor) = $crate::core::encode_cbor(&$value) {
            $crate::record($crate::core::EventKind::StateSnapshot {
                key: ($key).to_string(),
                cbor,
            });
        }
    }};
}
