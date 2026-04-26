#!/usr/bin/env node
// Example 1 (motoko) — Hello, trace.
//
// Mirror of 01-hello-trace.mjs against the Motoko escrow. The agent-side
// code is identical; only the canister id and the trace label change. The
// Motoko canister exposes the same Candid surface (TraceHeader as the
// first arg, __debug_drain : query () -> blob) and emits the same
// CBOR-encoded events, so collect/recorder/UI all stay unchanged.
//
// Run:  node examples/scripts/01-hello-trace-motoko.mjs

import { HttpAgent, Actor } from "@dfinity/agent";
import { IDL } from "@dfinity/candid";
import { Principal } from "@dfinity/principal";
import { readFileSync } from "node:fs";

import { newTrace } from "../../agent-js/dist/index.js";

const RECORDER = "http://127.0.0.1:9191";
const REPLICA  = "http://127.0.0.1:8000";
const ids = JSON.parse(readFileSync(".icp/cache/mappings/local.ids.json", "utf8"));

const Header = IDL.Record({
  trace_id:    IDL.Vec(IDL.Nat8),
  parent_seq:  IDL.Nat64,
  parent_span: IDL.Nat64,
});

// Motoko's lock_funds takes `?TraceHeader` (opt) so we wrap the header
// in an opt vector. Everything else is identical to the Rust example.
const escrowIdl = ({ IDL }) =>
  IDL.Service({
    lock_funds:    IDL.Func([IDL.Opt(Header), IDL.Nat64, IDL.Nat64], [IDL.Reserved], []),
    __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
  });

const agent  = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const escrow = Actor.createActor(escrowIdl, {
  agent,
  canisterId: Principal.fromText(ids.motoko_escrow),
});

const trace = await newTrace(RECORDER, "example 1 (motoko): hello trace");
console.log(`trace id: ${trace.id}`);

await escrow.lock_funds([trace.header()], 1n, 100n);

const blob  = await escrow.__debug_drain();
const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
await fetch(`${RECORDER}/drain`, {
  method: "POST",
  headers: { "content-type": "application/cbor" },
  body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
});

console.log(`drained ${bytes.byteLength} bytes`);
console.log(`→ open http://127.0.0.1:9192 and click "example 1 (motoko): hello trace"`);
