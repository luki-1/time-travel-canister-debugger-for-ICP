/// Minimal CBOR encoder for the ic-debug Event schema.
///
/// We hand-roll a tight encoder for the exact wire format that
/// `ciborium::from_reader::<Vec<Event>>` expects on the Rust recorder
/// (`POST /drain`). Notable conventions matched:
///
///   * Internally-tagged `EventKind` (`#[serde(tag = "kind", rename_all = "snake_case")]`)
///     — variant tag is flattened into each variant's map as a `kind` text key.
///   * `args_hash` and `cbor` use `serde_bytes` → CBOR major type 2 (byte string),
///     not arrays of integers.
///   * `trace_id` is the serde Uuid default → 16 raw bytes (CBOR major type 2).
///   * `Option<T>` is null or the inner value.
///   * `ts_nanos` (Rust u128) is emitted as a u64 — well within range for
///     `ic0.time()` and CBOR-decodes cleanly into u128 on the Rust side.

import Buffer "mo:base/Buffer";
import Nat8 "mo:base/Nat8";
import Nat64 "mo:base/Nat64";
import Nat32 "mo:base/Nat32";
import Text "mo:base/Text";
import Blob "mo:base/Blob";
import Char "mo:base/Char";

import Types "./Types";

module {

  type Buf = Buffer.Buffer<Nat8>;

  // ---------- Public ----------

  /// CBOR-encode a `Vec<Event>`.
  public func encodeEvents(events : [Types.Event]) : Blob {
    let buf = Buffer.Buffer<Nat8>(256);
    encArrayHead(buf, Nat64.fromNat(events.size()));
    for (ev in events.vals()) encEvent(buf, ev);
    Blob.fromArray(Buffer.toArray(buf))
  };

  /// Format 16 raw bytes as a hyphenated lowercase UUID (Uuid serde default).
  /// Pads or truncates to 16 bytes if needed.
  public func formatUuid(b : Blob) : Text {
    let bytes = Blob.toArray(b);
    let n = bytes.size();
    var out = "";
    var i : Nat = 0;
    while (i < 16) {
      let byte : Nat8 = if (i < n) bytes[i] else 0;
      out #= Char.toText(nibble(byte >> 4));
      out #= Char.toText(nibble(byte & 0x0F));
      if (i == 3 or i == 5 or i == 7 or i == 9) out #= "-";
      i += 1;
    };
    out
  };

  /// Wrap a Text value as a CBOR text string. Useful so user state snapshots
  /// can be embedded in `StateSnapshot.cbor` and remain valid CBOR for the
  /// replay UI to decode.
  public func encodeText(s : Text) : Blob {
    let buf = Buffer.Buffer<Nat8>(s.size() + 5);
    encText(buf, s);
    Blob.fromArray(Buffer.toArray(buf))
  };

  // ---------- Event encoding ----------

  func encEvent(buf : Buf, ev : Types.Event) {
    encMapHead(buf, 7);
    encText(buf, "trace_id");   encBytes(buf, ev.trace_id);
    encText(buf, "seq");        encUint(buf, ev.seq);
    encText(buf, "parent_seq"); encOptUint(buf, ev.parent_seq);
    encText(buf, "span_id");    encUint(buf, ev.span_id);
    encText(buf, "ts_nanos");   encUint(buf, ev.ts_nanos);
    encText(buf, "canister");
    switch (ev.canister) { case null encNull(buf); case (?s) encText(buf, s) };
    encText(buf, "kind");       encEventKind(buf, ev.kind);
  };

  func encEventKind(buf : Buf, k : Types.EventKind) {
    switch (k) {
      case (#IngressEntered { method; caller; args_hash }) {
        encMapHead(buf, 4);
        encText(buf, "kind");      encText(buf, "ingress_entered");
        encText(buf, "method");    encText(buf, method);
        encText(buf, "caller");    encText(buf, caller);
        encText(buf, "args_hash"); encBytes(buf, args_hash);
      };
      case (#MethodEntered { method; caller; args }) {
        encMapHead(buf, 4);
        encText(buf, "kind");   encText(buf, "method_entered");
        encText(buf, "method"); encText(buf, method);
        encText(buf, "caller"); encText(buf, caller);
        encText(buf, "args");   encArgs(buf, args);
      };
      case (#MethodExited { reject }) {
        encMapHead(buf, 2);
        encText(buf, "kind");   encText(buf, "method_exited");
        encText(buf, "reject"); encOptText(buf, reject);
      };
      case (#CallSpawned { target; method; args_hash }) {
        encMapHead(buf, 4);
        encText(buf, "kind");      encText(buf, "call_spawned");
        encText(buf, "target");    encText(buf, target);
        encText(buf, "method");    encText(buf, method);
        encText(buf, "args_hash"); encBytes(buf, args_hash);
      };
      case (#CallReturned { reject }) {
        encMapHead(buf, 2);
        encText(buf, "kind");   encText(buf, "call_returned");
        encText(buf, "reject"); encOptText(buf, reject);
      };
      case (#StateSnapshot { key; cbor }) {
        encMapHead(buf, 3);
        encText(buf, "kind"); encText(buf, "state_snapshot");
        encText(buf, "key");  encText(buf, key);
        encText(buf, "cbor"); encBytes(buf, cbor);
      };
      case (#TimerFired { label_ }) {
        encMapHead(buf, 2);
        encText(buf, "kind");  encText(buf, "timer_fired");
        encText(buf, "label"); encText(buf, label_);
      };
      case (#Note { label_ }) {
        encMapHead(buf, 2);
        encText(buf, "kind");  encText(buf, "note");
        encText(buf, "label"); encText(buf, label_);
      };
    };
  };

  func encArgs(buf : Buf, args : [Types.Arg]) {
    encArrayHead(buf, Nat64.fromNat(args.size()));
    for ((name, val) in args.vals()) {
      encArrayHead(buf, 2);
      encText(buf, name);
      encText(buf, val);
    };
  };

  // ---------- Primitives ----------

  func encOptUint(buf : Buf, x : ?Nat64) {
    switch (x) { case null encNull(buf); case (?n) encUint(buf, n) };
  };

  func encOptText(buf : Buf, x : ?Text) {
    switch (x) { case null encNull(buf); case (?s) encText(buf, s) };
  };

  func encUint(buf : Buf, n : Nat64) { encHead(buf, 0, n) };

  func encArrayHead(buf : Buf, n : Nat64) { encHead(buf, 4, n) };

  func encMapHead(buf : Buf, n : Nat64) { encHead(buf, 5, n) };

  func encNull(buf : Buf) { buf.add(0xF6) };

  func encText(buf : Buf, s : Text) {
    let bytes = Blob.toArray(Text.encodeUtf8(s));
    encHead(buf, 3, Nat64.fromNat(bytes.size()));
    for (b in bytes.vals()) buf.add(b);
  };

  func encBytes(buf : Buf, b : Blob) {
    let bytes = Blob.toArray(b);
    encHead(buf, 2, Nat64.fromNat(bytes.size()));
    for (x in bytes.vals()) buf.add(x);
  };

  /// Emit a CBOR initial byte: top 3 bits = major type (0..7),
  /// low 5 bits = length encoding (immediate or 1/2/4/8 trailing bytes BE).
  func encHead(buf : Buf, mt : Nat8, n : Nat64) {
    let prefix : Nat8 = mt << 5;
    if (n < 24) {
      buf.add(prefix | byte(n));
    } else if (n < 0x100) {
      buf.add(prefix | 24);
      buf.add(byte(n));
    } else if (n < 0x10000) {
      buf.add(prefix | 25);
      encBE(buf, n, 2);
    } else if (n < 0x100000000) {
      buf.add(prefix | 26);
      encBE(buf, n, 4);
    } else {
      buf.add(prefix | 27);
      encBE(buf, n, 8);
    };
  };

  func encBE(buf : Buf, n : Nat64, width : Nat) {
    var i : Nat = width;
    while (i > 0) {
      i -= 1;
      let shift : Nat64 = Nat64.fromNat(i * 8);
      buf.add(byte((n >> shift) & 0xFF));
    };
  };

  func byte(n : Nat64) : Nat8 {
    Nat8.fromNat(Nat64.toNat(n & 0xFF))
  };

  // 0..15 → '0'..'9' or 'a'..'f'
  func nibble(n : Nat8) : Char {
    let c : Nat32 = Nat32.fromNat(Nat8.toNat(n));
    if (c < 10) Char.fromNat32(c + 0x30)
    else Char.fromNat32(c + 0x57)
  };

}
