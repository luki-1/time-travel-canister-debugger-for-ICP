#!/usr/bin/env node
// Example 4 — Silent error: failure with no flagging notes.
//
// Uses a dedicated `ledger` canister that stores named account balances.
// The happy path (deposit) is fully instrumented and leaves a clear trace.
// The failure path (transfer with insufficient funds) is not: the canister
// returns `false` and exits without emitting any trace_event!, so the UI
// has nothing to flag.
//
// Both calls share one trace, which makes the contrast visible in a single
// timeline:
//
//   ▶ ENTER  deposit
//   · NOTE   ledger.deposit:enter
//   ● STATE  account/alice-<run> = 50    ← alice funded
//   ◀ EXIT
//   ▶ ENTER  transfer
//   · NOTE   ledger.transfer:enter
//   ◀ EXIT                               ← clean exit, no ⚠, no STATE
//
// The account names include a short run suffix derived from the trace id so
// every run starts from a clean slate — the ledger canister accumulates
// deposits (`*bal += amount`), so reusing the same name across runs would
// make alice's balance grow unboundedly.
//
// What to look for in the UI:
//   1. The trace header shows NO ⚠ pill — zero flagged events.
//   2. Stepping through the timeline, the state panel shows account/alice-…=50
//      after the deposit, then nothing changes after the transfer exits.
//   3. There is no account/bob-… entry — it was never created.
//   4. The only signal is the absence of the STATE events a successful
//      transfer would have emitted.
//
// The fix in the canister is one line in the `else` branch:
//   trace_event!("ledger.transfer:failed");   // ← now auto-flagged
//
// Run:  node examples/scripts/04-silent-error.mjs

import { HttpAgent, Actor } from "@dfinity/agent";
import { IDL } from "@dfinity/candid";
import { Principal } from "@dfinity/principal";
import { readFileSync } from "node:fs";

import { newTrace } from "../../agent-js/dist/index.js";

const RECORDER = "http://127.0.0.1:9191";
const REPLICA  = "http://127.0.0.1:8000";
const ids = JSON.parse(readFileSync(".icp/cache/mappings/local.ids.json", "utf8"));

const Header = IDL.Record({
  trace_id:   IDL.Vec(IDL.Nat8),
  parent_seq: IDL.Nat64,
  parent_span: IDL.Nat64,
});

const ledgerIdl = ({ IDL }) => IDL.Service({
  deposit:       IDL.Func([Header, IDL.Text, IDL.Nat64], [],          []),
  transfer:      IDL.Func([Header, IDL.Text, IDL.Text, IDL.Nat64], [IDL.Bool], []),
  balance:       IDL.Func([IDL.Text], [IDL.Opt(IDL.Nat64)],           ["query"]),
  __debug_drain: IDL.Func([],         [IDL.Vec(IDL.Nat8)],            ["query"]),
});

const agent  = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });
const ledger = Actor.createActor(ledgerIdl, {
  agent,
  canisterId: Principal.fromText(ids.ledger),
});

const trace = await newTrace(RECORDER, "example 4 (rust): silent error");
console.log(`trace id: ${trace.id}`);

// Unique per-run suffix so repeat runs don't accumulate on the same accounts.
const run = trace.id.slice(0, 8);
const alice = `alice-${run}`;
const bob   = `bob-${run}`;

// 1. Deposit 50 to alice — succeeds, emits state snapshot.
await ledger.deposit(trace.header(), alice, 50n);
console.log(`deposited 50 → ${alice}`);

// 2. Transfer 200 from alice — fails silently (alice only has 50).
//    The canister returns false and exits without a trace_event!.
const ok = await ledger.transfer(trace.header(), alice, bob, 200n);
console.log(`transfer ${alice}→${bob} 200: ${ok}`);   // false

// Confirm state: alice unchanged, bob never created.
const aliceBal = await ledger.balance(alice);
const bobBal   = await ledger.balance(bob);
console.log(`${alice} balance: ${aliceBal[0] ?? "none"}`);
console.log(`${bob} balance: ${bobBal[0]   ?? "none"}`);

// Drain — only the ledger canister participated.
const blob  = await ledger.__debug_drain();
const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
await fetch(`${RECORDER}/drain`, {
  method: "POST",
  headers: { "content-type": "application/cbor" },
  body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
});
console.log(`drained ${bytes.byteLength} bytes`);
console.log(``);
console.log(`→ open http://127.0.0.1:9192 → "example 4 (rust): silent error"`);
console.log(`  no ⚠ pill, no red rows — the transfer looks like it worked.`);
console.log(`  step to the second EXIT and check the state panel:`);
console.log(`  account/${alice} is still 50, account/${bob} was never created.`);
