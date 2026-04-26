#!/usr/bin/env node
// Example 4 (motoko) — Silent error: failure with no flagging notes.
//
// Mirror of 04-silent-error.mjs against the Motoko ledger. Same shape:
// deposit succeeds and emits a state snapshot; transfer fails silently
// (insufficient funds branch emits no note), so the timeline shows a
// clean exit with no ⚠ — only the missing STATE events betray the bug.
//
// Run:  node examples/scripts/04-silent-error-motoko.mjs

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

const ledgerIdl = ({ IDL }) => IDL.Service({
  deposit:       IDL.Func([IDL.Opt(Header), IDL.Text, IDL.Nat64], [],          []),
  transfer:      IDL.Func([IDL.Opt(Header), IDL.Text, IDL.Text, IDL.Nat64], [IDL.Bool], []),
  balance:       IDL.Func([IDL.Text], [IDL.Opt(IDL.Nat64)], ["query"]),
  __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
});

const agent  = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const ledger = Actor.createActor(ledgerIdl, {
  agent,
  canisterId: Principal.fromText(ids.motoko_ledger),
});

const trace = await newTrace(RECORDER, "example 4 (motoko): silent error");
console.log(`trace id: ${trace.id}`);

const run = trace.id.slice(0, 8);
const alice = `alice-${run}`;
const bob   = `bob-${run}`;

await ledger.deposit([trace.header()], alice, 50n);
console.log(`deposited 50 → ${alice}`);

const ok = await ledger.transfer([trace.header()], alice, bob, 200n);
console.log(`transfer ${alice}→${bob} 200: ${ok}`);

const aliceBal = await ledger.balance(alice);
const bobBal   = await ledger.balance(bob);
console.log(`${alice} balance: ${aliceBal[0] ?? "none"}`);
console.log(`${bob} balance: ${bobBal[0]   ?? "none"}`);

const blob  = await ledger.__debug_drain();
const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
await fetch(`${RECORDER}/drain`, {
  method: "POST",
  headers: { "content-type": "application/cbor" },
  body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
});
console.log(`drained ${bytes.byteLength} bytes`);
console.log(``);
console.log(`→ open http://127.0.0.1:9192 → "example 4 (motoko): silent error"`);
console.log(`  no ⚠ pill, no red rows — the transfer looks like it worked.`);
console.log(`  step to the second EXIT and check the state panel:`);
console.log(`  account/${alice} is still 50, account/${bob} was never created.`);
