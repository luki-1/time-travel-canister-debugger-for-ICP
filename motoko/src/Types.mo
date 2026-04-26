/// Shared trace event types for ic-debug — Motoko port.
///
/// These mirror `crates/ic-debug-core/src/lib.rs`. The wire format on the
/// `__debug_drain` query is CBOR (matching `ciborium`'s serde defaults), so
/// the existing Rust CLI recorder ingests Motoko-emitted events without
/// changes. Inter-canister `TraceHeader` propagation uses Candid, matching
/// the Rust `#[derive(CandidType)]` layout.

module {

  public type TraceId = Blob;     // 16 raw bytes (serde Uuid default in CBOR)
  public type SpanId  = Nat64;
  public type Seq     = Nat64;

  /// Propagated as the first positional argument of every traced
  /// inter-canister call so the callee can attach to the same trace.
  /// Candid layout: `record { trace_id: blob; parent_seq: nat64; parent_span: nat64 }`.
  public type TraceHeader = {
    trace_id    : Blob;   // 16 raw bytes
    parent_seq  : Seq;
    parent_span : SpanId;
  };

  public type Arg = (Text, Text);

  public type EventKind = {
    #IngressEntered : { method : Text; caller : Text; args_hash : Blob };
    #MethodEntered  : { method : Text; caller : Text; args : [Arg] };
    #MethodExited   : { reject : ?Text };
    #CallSpawned    : { target : Text; method : Text; args_hash : Blob };
    #CallReturned   : { reject : ?Text };
    #StateSnapshot  : { key : Text; cbor : Blob };
    #TimerFired     : { label_ : Text };
    #Note           : { label_ : Text };
  };

  public type Event = {
    trace_id   : TraceId;
    seq        : Seq;
    parent_seq : ?Seq;
    span_id    : SpanId;
    ts_nanos   : Nat64;
    canister   : ?Text;     // canister principal in textual form
    kind       : EventKind;
  };

}
