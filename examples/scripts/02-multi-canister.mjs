#!/usr/bin/env node
// Example 2 — One trace, many canisters.
//
// Shows what happens when a traced call spawns inter-canister calls.
// One `newTrace(...)` on the agent side becomes one left-rail entry in
// the UI, with events from all three sample canisters interleaved in
// causal order.
//
// The ic-debug-specific code is identical to example 1. The point of this
// example is the draining loop at the end — every canister that might have
// participated needs its buffer pulled.
//
// Run:  node agent-js/examples/02-multi-canister.mjs

import { HttpAgent, Actor } from "@dfinity/agent";
import { IDL } from "@dfinity/candid";
import { Principal } from "@dfinity/principal";
import { readFileSync } from "node:fs";

import { newTrace } from "../dist/index.js";

const RECORDER = "http://127.0.0.1:9191";
const REPLICA  = "http://127.0.0.1:8000";
const ids = JSON.parse(readFileSync(".icp/cache/mappings/local.ids.json", "utf8"));

const Header = IDL.Record({
  trace_id:   IDL.Vec(IDL.Nat8),
  parent_seq: IDL.Nat64,
  parent_span: IDL.Nat64,
});

// Every canister with #[trace_method] methods also exposes __debug_drain.
// A tiny IDL that covers just the drain is enough to loop over all three.
const drainIdl = ({ IDL }) => IDL.Service({
  __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});

const frontendIdl = ({ IDL }) => IDL.Service({
  configure:      IDL.Func([IDL.Principal, IDL.Principal], [], []),
  submit_payment: IDL.Func([Header, IDL.Nat64], [IDL.Reserved], []),
  __debug_drain:  IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});

const agent    = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const frontend = Actor.createActor(frontendIdl, {
  agent,
  canisterId: Principal.fromText(ids.frontend_api),
});
await frontend.configure(
  Principal.fromText(ids.escrow),
  Principal.fromText(ids.notifications),
);

// 1. Start the trace, same as example 1.
const trace = await newTrace(RECORDER, "example 2: multi-canister");
console.log(`trace id: ${trace.id}`);

// 2. Make the call. Internally submit_payment uses call_traced! to hit
//    escrow.lock_funds and notifications.send_receipt. The macro builds
//    a *child* TraceHeader pointing at its own seq and prepends it to
//    each call's candid args — that's how causality threads across
//    canister boundaries without any extra agent-side code.
await frontend.submit_payment(trace.header(), 250n);

// 3. Drain every participant. Any canister that didn't emit events
//    returns a zero-byte blob, which we skip. Order doesn't matter — the
//    recorder interleaves by (trace_id, canister, seq).
const canisters = {
  frontend_api:  ids.frontend_api,
  escrow:        ids.escrow,
  notifications: ids.notifications,
};
for (const [name, cid] of Object.entries(canisters)) {
  const actor = Actor.createActor(drainIdl, { agent, canisterId: Principal.fromText(cid) });
  const blob  = await actor.__debug_drain();
  const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
  if (bytes.byteLength === 0) { console.log(`  ${name}: (empty)`); continue; }
  await fetch(`${RECORDER}/drain`, {
    method: "POST",
    headers: { "content-type": "application/cbor" },
    body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
  });
  console.log(`  ${name}: ${bytes.byteLength} bytes`);
}

console.log(`→ open http://127.0.0.1:9192 and watch one trace unroll across`);
console.log(`  all three canisters. The colored canister tag on each row`);
console.log(`  is the principal hash — it changes every time the call`);
console.log(`  crosses a boundary.`);
