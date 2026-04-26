#!/usr/bin/env node
// Example 3 (motoko) — Flagging an error.
//
// Mirror of 03-flag-an-error.mjs against the Motoko pipeline. Arms the
// Motoko notifications canister to reject the next send_receipt; the
// motoko_frontend_api's intentional rollback bug then leaves the payment
// stuck in `Locked` — same shape as the Rust demo, surfaced through the
// same UI flag heuristic.
//
// Run:  node examples/scripts/03-flag-an-error-motoko.mjs

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

const frontendIdl = ({ IDL }) => IDL.Service({
  configure:      IDL.Func([IDL.Principal, IDL.Principal], [], []),
  submit_payment: IDL.Func([IDL.Opt(Header), IDL.Nat64], [IDL.Reserved], []),
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
  agent, canisterId: Principal.fromText(ids.motoko_frontend_api),
});
const notifications = Actor.createActor(notificationsIdl, {
  agent, canisterId: Principal.fromText(ids.motoko_notifications),
});
await frontend.configure(
  Principal.fromText(ids.motoko_escrow),
  Principal.fromText(ids.motoko_notifications),
);

await notifications.arm_failure();

const trace = await newTrace(RECORDER, "example 3 (motoko): flag an error");
console.log(`trace id: ${trace.id}`);

await frontend.submit_payment([trace.header()], 500n);

const participants = {
  motoko_frontend_api:  ids.motoko_frontend_api,
  motoko_escrow:        ids.motoko_escrow,
  motoko_notifications: ids.motoko_notifications,
};
for (const [name, cid] of Object.entries(participants)) {
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
console.log(`→ open http://127.0.0.1:9192 → "example 3 (motoko): flag an error"`);
console.log(`  expect a red "⚠" pill; press \`n\` to cycle through flagged events.`);
