// ic-debug-agent-js
//
// Minimal Milestone 1 surface:
//   * `newTrace(recorderUrl, label)` — registers a trace with the recorder
//     and returns `{ id, header() }`. Pass `header()` as the first argument
//     to any instrumented canister method.
//   * `logEvent(recorderUrl, event)`  — POST a single agent-side event.
//   * `postDrain(recorderUrl, bytes)` — POST a CBOR drain blob from a
//     canister's `__debug_drain` query.
//
// Full `HttpAgent` wrapping (automatic header injection + ingress capture)
// is deferred — it requires re-encoding Candid args and isn't necessary
// when the canister methods accept `TraceHeader` as the first positional.

export interface TraceHeader {
  /** 16-byte UUID as a Uint8Array — matches the Candid `blob` repr. */
  trace_id: Uint8Array;
  parent_seq: bigint;
  parent_span: bigint;
}

export interface Trace {
  /** Canonical UUID in textual form, also returned by the recorder. */
  id: string;
  /** A fresh `TraceHeader` to pass as the first arg to instrumented calls. */
  header(): TraceHeader;
  /** Flush any in-memory state. No-op in Milestone 1. */
  finish(): Promise<void>;
}

export async function newTrace(recorderUrl: string, label?: string): Promise<Trace> {
  const id = crypto.randomUUID();
  const res = await fetch(`${recorderUrl.replace(/\/$/, "")}/traces`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ trace_id: id, label }),
  });
  if (!res.ok) {
    throw new Error(`register trace failed: ${res.status} ${await res.text()}`);
  }
  const traceBytes = uuidToBytes(id);
  return {
    id,
    header: () => ({
      trace_id: traceBytes,
      parent_seq: 0n,
      parent_span: 0n,
    }),
    async finish() {
      // Intentionally empty; the recorder flushes on every write.
    },
  };
}

export type AgentEvent =
  | { kind: "ingress_entered"; method: string; caller: string; args_hash: string }
  | { kind: "note"; label: string };

export async function logEvent(
  recorderUrl: string,
  traceId: string,
  kind: AgentEvent,
): Promise<void> {
  const ev = {
    trace_id: traceId,
    seq: 0,
    parent_seq: null,
    span_id: 0,
    ts_nanos: Date.now() * 1_000_000,
    canister: null,
    kind,
  };
  const res = await fetch(`${recorderUrl.replace(/\/$/, "")}/events`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify([ev]),
  });
  if (!res.ok) throw new Error(`logEvent failed: ${res.status}`);
}

/** POST the CBOR blob returned by a canister's `__debug_drain` query. */
export async function postDrain(recorderUrl: string, cbor: Uint8Array): Promise<void> {
  const res = await fetch(`${recorderUrl.replace(/\/$/, "")}/drain`, {
    method: "POST",
    headers: { "content-type": "application/cbor" },
    body: cbor.buffer.slice(cbor.byteOffset, cbor.byteOffset + cbor.byteLength) as ArrayBuffer,
  });
  if (!res.ok) throw new Error(`postDrain failed: ${res.status}`);
}

function uuidToBytes(id: string): Uint8Array {
  const hex = id.replace(/-/g, "");
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
