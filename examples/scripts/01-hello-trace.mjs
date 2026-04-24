#!/usr/bin/env node
// Example 1 — Hello, trace.
//
// The absolute minimum to record a canister call with ic-debug.
// Exactly one canister, one method, one drain. Read top-to-bottom; every
// line is commented.
//
// The call we make is `escrow.lock_funds(header, payment_id, amount)` —
// it's a single-canister operation with no inter-canister fan-out, which
// keeps the example focused on the three ic-debug primitives: register a
// trace, pass the header, drain.
//
// Prereqs: `icp network start && icp deploy` + `ic-debug record` on :9191.
// Run from repo root:   node agent-js/examples/01-hello-trace.mjs

import { HttpAgent, Actor } from "@dfinity/agent";
import { IDL } from "@dfinity/candid";
import { Principal } from "@dfinity/principal";
import { readFileSync } from "node:fs";

// newTrace is the only ic-debug helper this example needs.
// It registers a trace id with the recorder and returns a { id, header() }
// handle. header() is what you pass as the first arg to any instrumented
// canister method.
import { newTrace } from "../dist/index.js";

const RECORDER = "http://127.0.0.1:9191";          // where `ic-debug record` listens
const REPLICA  = "http://127.0.0.1:8000";          // local icp network

// Canister ids come from the icp-cli deploy mapping. No magic.
const ids = JSON.parse(readFileSync(".icp/cache/mappings/local.ids.json", "utf8"));

// --- Candid — only the fields/methods this example touches. ---
// TraceHeader is the 3-field record every #[trace_method] entrypoint takes
// as its first positional arg.
const Header = IDL.Record({
  trace_id:   IDL.Vec(IDL.Nat8),
  parent_seq: IDL.Nat64,
  parent_span: IDL.Nat64,
});

// escrow.lock_funds returns a Lock record; we don't read the reply, so
// IDL.Reserved is enough to satisfy the decoder without forcing us to
// define the full shape here.
const escrowIdl = ({ IDL }) =>
  IDL.Service({
    lock_funds:    IDL.Func([Header, IDL.Nat64, IDL.Nat64], [IDL.Reserved], []),
    __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
  });

// 1. Build an agent + actor like any dfinity/agent project.
const agent  = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const escrow = Actor.createActor(escrowIdl, {
  agent,
  canisterId: Principal.fromText(ids.escrow),
});

// 2. Start a trace. The label is what shows up in the UI's left rail.
const trace = await newTrace(RECORDER, "example 1: hello trace");
console.log(`trace id: ${trace.id}`);

// 3. Make the traced call. The only ic-debug-specific bit is
//    `trace.header()` as the first argument. The canister's
//    #[trace_method] adopts it and every event emitted inside
//    lock_funds is now tagged with this trace id.
//
//    lock_funds(payment_id, amount): creates a Lock record for payment
//    #1 holding 100 units. Internally it emits:
//      - method_entered  (from #[trace_method])
//      - note "escrow.lock_funds:enter"
//      - state_snapshot  "lock" = { payment_id: 1, amount: 100, released: false }
//      - method_exited
await escrow.lock_funds(trace.header(), 1n, 100n);

// 4. Drain. The canister buffers events in memory; the agent pulls the
//    CBOR blob and POSTs it to the recorder. Because lock_funds runs
//    entirely inside one canister, only that canister has events to
//    drain. (Example 2 covers the multi-canister drain pattern.)
const blob = await escrow.__debug_drain();
const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
await fetch(`${RECORDER}/drain`, {
  method: "POST",
  headers: { "content-type": "application/cbor" },
  body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
});

console.log(`drained ${bytes.byteLength} bytes`);
console.log(`→ open http://127.0.0.1:9192 and click "example 1: hello trace"`);
