#!/usr/bin/env node
// Example 3 — Flagging an error.
//
// Produces a failing flow and shows where the debugger surfaces it.
//
// The sample notifications canister has an `arm_failure()` entrypoint that
// makes its next `send_receipt` call reject. frontend_api.submit_payment
// has an intentional bug: when send_receipt rejects, it logs a
// `rollback_missing` note and returns without undoing the escrow lock.
//
// What to look for in the UI after running this:
//   1. The trace row in the left rail has no special marker (yet).
//   2. Click into it — the header shows a red "⚠ 2 errors" pill.
//   3. Press `n` to jump to the first flagged event. The row is tinted red
//      and the right-hand panel shows a "⚠ flagged: …" banner with the
//      reason.
//   4. The state panel shows payment.status stuck at "Locked" — the bug.
//
// Run:  node agent-js/examples/03-flag-an-error.mjs

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

const frontendIdl = ({ IDL }) => IDL.Service({
  configure:      IDL.Func([IDL.Principal, IDL.Principal], [], []),
  submit_payment: IDL.Func([Header, IDL.Nat64], [IDL.Reserved], []),
  __debug_drain:  IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});
const notificationsIdl = ({ IDL }) => IDL.Service({
  arm_failure:   IDL.Func([], [], []),
  __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});
const drainIdl = ({ IDL }) => IDL.Service({
  __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});

const agent = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const frontend = Actor.createActor(frontendIdl, {
  agent, canisterId: Principal.fromText(ids.frontend_api),
});
const notifications = Actor.createActor(notificationsIdl, {
  agent, canisterId: Principal.fromText(ids.notifications),
});
await frontend.configure(
  Principal.fromText(ids.escrow),
  Principal.fromText(ids.notifications),
);

// 1. Prime the failure. The next send_receipt the canister sees will reject.
await notifications.arm_failure();

// 2. Trace as usual — nothing special needed to "capture" the failure.
//    The canister emits a normal reject event; the UI recognises the
//    reject kind and any `trace_event!` note whose label contains words
//    like "fail", "reject", or "rollback".
const trace = await newTrace(RECORDER, "example 3: flag an error");
console.log(`trace id: ${trace.id}`);

// Candid-level this looks like a successful call. At the IC level the
// *inter-canister* call to send_receipt rejects — which is exactly the
// kind of bug that's easy to miss without a time-travel debugger.
await frontend.submit_payment(trace.header(), 500n);

// 3. Drain.
for (const [name, cid] of Object.entries(ids)) {
  if (!["frontend_api", "escrow", "notifications"].includes(name)) continue;
  const actor = Actor.createActor(drainIdl, { agent, canisterId: Principal.fromText(cid) });
  const blob  = await actor.__debug_drain();
  const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
  if (bytes.byteLength === 0) continue;
  await fetch(`${RECORDER}/drain`, {
    method: "POST",
    headers: { "content-type": "application/cbor" },
    body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
  });
  console.log(`  ${name}: ${bytes.byteLength} bytes`);
}

console.log(``);
console.log(`→ open http://127.0.0.1:9192 → "example 3: flag an error"`);
console.log(`  expect a red "⚠ 2 errors" pill; press \`n\` to cycle through them.`);
