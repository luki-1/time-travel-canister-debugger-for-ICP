/// Canister-side instrumentation for ic-debug — Motoko port of
/// `crates/ic-debug-trace`. Same wire format and same `__debug_drain`
/// query contract, so the existing Rust CLI recorder and `collect.mjs`
/// ingest Motoko events without changes.
///
/// Typical use inside an actor:
///
/// ```motoko
/// import Trace "../../../motoko/src/Trace";
///
/// actor self {
///   let tracer = Trace.Tracer(Principal.fromActor(self));
///
///   public shared(msg) func lock_funds(
///     header     : ?Trace.TraceHeader,
///     paymentId  : Nat64,
///     amount     : Nat64,
///   ) : async Lock {
///     ignore tracer.beginTrace(header);
///     tracer.methodEntered("lock_funds", msg.caller, [
///       ("payment_id", debug_show paymentId),
///       ("amount",     debug_show amount),
///     ]);
///     // ... body ...
///     tracer.methodExited(null);
///     lock
///   };
///
///   public query func __debug_drain() : async Blob { tracer.drain() };
/// }
/// ```

import Buffer "mo:base/Buffer";
import Time "mo:base/Time";
import Nat64 "mo:base/Nat64";
import Nat8 "mo:base/Nat8";
import Principal "mo:base/Principal";
import Blob "mo:base/Blob";
import Array "mo:base/Array";

import Types "./Types";
import Cbor "./Cbor";

module {

  public type TraceId    = Types.TraceId;
  public type SpanId     = Types.SpanId;
  public type Seq        = Types.Seq;
  public type TraceHeader = Types.TraceHeader;
  public type Event      = Types.Event;
  public type EventKind  = Types.EventKind;

  /// Per-canister tracer. One instance lives in the actor's body. Holds
  /// the active trace context and a bounded ring buffer of recorded events.
  public class Tracer(canisterId : Principal) {

    let CAP : Nat = 4096;
    let buf = Buffer.Buffer<Event>(CAP);

    var currentTraceBytes : ?Blob = null;     // 16 raw bytes — used for both events and outgoing headers
    var currentSpan : Nat64 = 0;
    var nextSpan : Nat64 = 0;
    var nextSeq : Nat64 = 0;
    var lastSeq : ?Nat64 = null;
    var counter : Nat64 = 0;

    let canisterText = Principal.toText(canisterId);

    // ---------- Trace lifecycle ----------

    /// Adopt an inbound `TraceHeader`, or mint a new trace if `null`.
    /// Returns the new span id.
    public func beginTrace(header : ?TraceHeader) : SpanId {
      switch (header) {
        case (?h) {
          currentTraceBytes := ?h.trace_id;
          lastSeq := ?h.parent_seq;
        };
        case null {
          currentTraceBytes := ?mintTraceId();
          lastSeq := null;
        };
      };
      nextSpan += 1;
      currentSpan := nextSpan;
      currentSpan
    };

    /// Build a `TraceHeader` that downstream callees can adopt. Returns
    /// `null` if no trace is active.
    public func currentHeader() : ?TraceHeader {
      switch (currentTraceBytes) {
        case null null;
        case (?bytes) ?{
          trace_id    = bytes;
          parent_seq  = switch (lastSeq) { case null 0; case (?s) s };
          parent_span = currentSpan;
        };
      };
    };

    // ---------- Event emitters ----------

    public func ingressEntered(method : Text, caller : Principal, argsHash : Blob) {
      record(#IngressEntered({
        method = method;
        caller = Principal.toText(caller);
        args_hash = argsHash;
      }));
    };

    public func methodEntered(method : Text, caller : Principal, args : [(Text, Text)]) {
      record(#MethodEntered({
        method = method;
        caller = Principal.toText(caller);
        args   = args;
      }));
    };

    public func methodExited(reject : ?Text) {
      record(#MethodExited({ reject = reject }));
    };

    public func note(label_ : Text) {
      record(#Note({ label_ = label_ }));
    };

    public func timerFired(label_ : Text) {
      record(#TimerFired({ label_ = label_ }));
    };

    public func snapshotBlob(key : Text, cbor : Blob) {
      record(#StateSnapshot({ key = key; cbor = cbor }));
    };

    /// Convenience: store a `debug_show`-style text rendering, wrapped as a
    /// CBOR text string so the replay UI sees a valid CBOR value.
    public func snapshotText(key : Text, value : Text) {
      record(#StateSnapshot({ key = key; cbor = Cbor.encodeText(value) }));
    };

    /// Emit `CallSpawned` and return the header to attach to the outgoing
    /// inter-canister call. The header's `parent_seq` points at this
    /// `CallSpawned` event so the callee's events form a back-edge.
    public func callSpawned(target : Principal, method : Text, argsHash : Blob) : TraceHeader {
      record(#CallSpawned({
        target = Principal.toText(target);
        method = method;
        args_hash = argsHash;
      }));
      switch (currentHeader()) {
        case (?h) h;
        case null {
          // Reachable only if record() was called outside an active trace —
          // record() short-circuits in that case, so the CallSpawned was
          // dropped. Hand back a nil header so the caller can still encode.
          {
            trace_id    = Blob.fromArray(Array.tabulate<Nat8>(16, func(_ : Nat) : Nat8 = 0));
            parent_seq  = 0;
            parent_span = 0;
          };
        };
      };
    };

    public func callReturned(reject : ?Text) {
      record(#CallReturned({ reject = reject }));
    };

    // ---------- Drain ----------

    /// Return CBOR-encoded `Vec<Event>` and clear the buffer. Wire it up as:
    /// `public query func __debug_drain() : async Blob { tracer.drain() }`.
    public func drain() : Blob {
      let events = Buffer.toArray(buf);
      buf.clear();
      Cbor.encodeEvents(events)
    };

    public func size() : Nat { buf.size() };

    // ---------- Internals ----------

    func record(kind : EventKind) {
      let trace = switch (currentTraceBytes) {
        case null { return };  // dropped: no active trace context
        case (?t) t;
      };
      let seq = nextSeq;
      nextSeq += 1;
      let parent = lastSeq;
      lastSeq := ?seq;
      let ev : Event = {
        trace_id   = trace;
        seq        = seq;
        parent_seq = parent;
        span_id    = currentSpan;
        ts_nanos   = now64();
        canister   = ?canisterText;
        kind       = kind;
      };
      if (buf.size() >= CAP) ignore buf.remove(0);
      buf.add(ev);
    };

    func now64() : Nat64 {
      // Time.now() returns nanoseconds since epoch as Int. ic0.time fits in u64.
      Nat64.fromIntWrap(Time.now())
    };

    /// Mint a 16-byte trace id from (time, monotonic counter). Not a true
    /// UUIDv4 but format-compatible (16 bytes — Rust side renders to text via
    /// `Uuid::to_string()` after CBOR-decoding).
    func mintTraceId() : Blob {
      counter += 1;
      let t = now64();
      let c = counter;
      let bytes = Array.tabulate<Nat8>(16, func(i : Nat) : Nat8 {
        let n : Nat64 =
          if (i < 8) {
            (t >> Nat64.fromNat((7 - i) * 8)) & 0xFF
          } else {
            (c >> Nat64.fromNat((15 - i) * 8)) & 0xFF
          };
        Nat8.fromNat(Nat64.toNat(n))
      });
      Blob.fromArray(bytes)
    };

  };

}
