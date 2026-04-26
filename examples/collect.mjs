// Query __debug_drain on each canister and POST the CBOR blob to the recorder.
// Run from F:/ICP after `icp deploy` + a traced ingress call.

import { HttpAgent, Actor } from "@dfinity/agent";
import { IDL } from "@dfinity/candid";
import { Principal } from "@dfinity/principal";
import { readFileSync } from "node:fs";

const RECORDER = process.env.RECORDER_URL ?? "http://127.0.0.1:9191";
const REPLICA  = process.env.IC_HOST ?? "http://127.0.0.1:8000";

// Pull canister IDs from the icp-cli mapping (refreshes after every deploy),
// with env-var overrides as a fallback.
const MAPPING_PATH = process.env.IDS_PATH ?? ".icp/cache/mappings/local.ids.json";
const mapping = JSON.parse(readFileSync(MAPPING_PATH, "utf8"));
const CANISTERS = {
  // Rust pipeline
  escrow:               process.env.ESCROW               ?? mapping.escrow,
  notifications:        process.env.NOTIFICATIONS        ?? mapping.notifications,
  ledger:               process.env.LEDGER               ?? mapping.ledger,
  frontend_api:         process.env.FRONTEND_API         ?? mapping.frontend_api,
  // Motoko pipeline
  motoko_escrow:        process.env.MOTOKO_ESCROW        ?? mapping.motoko_escrow,
  motoko_notifications: process.env.MOTOKO_NOTIFICATIONS ?? mapping.motoko_notifications,
  motoko_ledger:        process.env.MOTOKO_LEDGER        ?? mapping.motoko_ledger,
  motoko_frontend_api:  process.env.MOTOKO_FRONTEND_API  ?? mapping.motoko_frontend_api,
};

const idl = ({ IDL }) =>
  IDL.Service({
    __debug_drain: IDL.Func([], [IDL.Vec(IDL.Nat8)], ["query"]),
  });

const agent = await HttpAgent.create({ host: REPLICA, shouldFetchRootKey: true });

let total = 0;
for (const [name, cid] of Object.entries(CANISTERS)) {
  if (!cid) {
    console.log(`[${name}] no canister id — skipped`);
    continue;
  }
  const actor = Actor.createActor(idl, { agent, canisterId: Principal.fromText(cid) });
  const blob = await actor.__debug_drain();
  const bytes = blob instanceof Uint8Array ? blob : Uint8Array.from(blob);
  console.log(`[${name}] ${bytes.byteLength} bytes drained`);
  if (bytes.byteLength === 0) continue;
  const res = await fetch(`${RECORDER}/drain`, {
    method: "POST",
    headers: { "content-type": "application/cbor" },
    body: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
  });
  const out = await res.json();
  console.log(`[${name}] recorder → ${JSON.stringify(out)}`);
  total += out.inserted ?? 0;
}
console.log(`total events ingested: ${total}`);
