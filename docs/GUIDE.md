# ic-debug — User Guide

A time-travel debugger for ICP canisters. Record every inter-canister call,
state snapshot, and rejection from a payment flow (or your own), then walk
through the causal timeline event-by-event and watch state evolve.

This guide walks you through:

1. [Running the reference demo](#1-running-the-reference-demo)
2. [Instrumenting your own canisters](#2-instrumenting-your-own-canisters)
3. [The CLI: `record` / `replay` / `diff` / `serve`](#3-the-cli)
4. [The web UI: timeline + state panel + playback](#4-the-web-ui)
5. [The event model](#5-the-event-model)
6. [Keyboard shortcuts](#6-keyboard-shortcuts)
7. [Troubleshooting](#7-troubleshooting)

---

## 1. Running the reference demo

The fastest way to see what the debugger does is run the bundled demo.
It exercises two flows — a happy path and a bug — and records each as a
separate trace. You'll be able to diff them in the web UI.

### Prerequisites

- Rust stable + `wasm32-unknown-unknown` target
- `icp` CLI — https://github.com/dfinity/icp-cli
- Node.js ≥ 20

### One-time setup

From the repo root:

```bash
# 1. Build the CLI and the agent-js bindings.
cargo build --release -p ic-debug-cli
npm --prefix agent-js install
npm --prefix agent-js run build
npm --prefix ui install
npm --prefix ui run build

# 2. Boot a local replica and deploy the three sample canisters.
#    icp.yaml lives in examples/, so deploy from there.
icp network start -d
cd examples && icp deploy --mode reinstall && cd ..
```

### Three moving pieces

ic-debug has three processes that talk to each other:

```
  canisters          agent-js            ic-debug record          ic-debug serve
  ─────────          ────────            ─────────────────         ────────────────
  buffer events  →   drain + POST   →    SQLite store      ←       HTTP + UI
                                         (traces/…sqlite)
```

Start the recorder and the read-only serve in two terminals:

```bash
# terminal A — accepts agent POSTs and canister drains
./target/release/ic-debug record --store traces/ic-debug.sqlite --port 9191

# terminal B — serves the UI + read API against the same store
./target/release/ic-debug serve  --store traces/ic-debug.sqlite --port 9192 --ui-dir ui/dist
```

### Run the example scripts

From the repo root (they read `.icp/cache/mappings/local.ids.json` which
`icp deploy` writes there after each deploy):

```bash
node examples/scripts/01-hello-trace.mjs
node examples/scripts/02-multi-canister.mjs
node examples/scripts/03-flag-an-error.mjs
node examples/scripts/04-silent-error.mjs
```

Each script records one labelled trace. Open **http://127.0.0.1:9192**
and you'll see all four in the left rail.

| Script | What it demonstrates |
|---|---|
| `01-hello-trace` | Baseline: `escrow.lock_funds` end-to-end, single canister, four events. |
| `02-multi-canister` | `submit_payment` fans out to escrow + notifications. One trace id, three canisters, one timeline. |
| `03-flag-an-error` | Arms a reject in notifications, then runs `submit_payment`. Payment strands at `Locked`. `submit_payment:rollback_missing` is the smoking gun. |
| `04-silent-error` | `escrow.release` on a non-existent lock — the canister exits clean with no note, so the UI shows no ⚠. Demonstrates the "silent failure" blind spot. |

### Reading the flagged-error trace (script 03)

The `frontend_api` canister has an intentional bug: if
`notifications.send_receipt` returns `Err(…)` (or the call rejects), the
`submit_payment` method records a `rollback_missing` note and returns
without rolling back the escrow lock. Standard IC tooling sees a
*successful* call (Candid `Result::Err` is not an IC-level reject), so the
bug is easy to miss in production.

The debugger makes it impossible to miss:

| Artefact | Happy path (02) | Failure path (03) |
|-----------|---|---|
| Final `payment.status` | `Completed` | **`Locked`** ← never cleared |
| State transitions | `Pending → Locked → Completed` | `Pending → Locked` |
| Escrow `lock.released` | `true` | **`false`** ← funds stranded |
| Diagnostic note | — | `submit_payment:rollback_missing` |

The right-hand state panel renders the diff live as you step. See also
[§4](#4-the-web-ui).

---

## 2. Instrumenting your own canisters

Minimum viable instrumentation is four steps.

### 2.1 Add the dependency

```toml
# Cargo.toml of your canister
[dependencies]
ic-debug-trace = { path = "../../crates/ic-debug-trace" }
```

### 2.2 Accept a `TraceHeader` and annotate the entrypoint

```rust
use ic_debug_trace::core::TraceHeader;
use ic_debug_trace::{call_traced, trace_event, trace_method, trace_state};

#[update]
#[trace_method]                                   // ← wraps with begin/end
async fn submit_payment(header: TraceHeader,      // ← first arg, adopted
                        amount: u64) -> Payment {
    let _ = header;                               // silence unused warning
    trace_event!("submit_payment:enter");
    // … business logic …
}
```

`#[trace_method]` injects a `begin_trace(header)` call at the top of the
method so the recorder knows which trace every `trace_event!`,
`trace_state!`, and `call_traced!` inside this span belongs to.

### 2.3 Capture state at interesting moments

```rust
trace_state!("payment", &payment);   // CBOR-encoded, diff'd later
```

Call it whenever a state transition happens. Each snapshot is keyed by
`(canister, key, seq)` — same key across multiple snapshots is how the
diff engine derives transitions.

### 2.4 Propagate through inter-canister calls

Use `call_traced!` in place of `ic_cdk::call`:

```rust
let (lock,): (Lock,) = call_traced!(escrow, "lock_funds", (id, amount))
    .expect("escrow.lock_funds failed");
```

The macro records a `CallSpawned` event, builds a child `TraceHeader`
pointing at this call's seq as the parent, and prepends it to the
encoded args. The callee receives the header as its first positional
arg (requirement: annotate that method with `#[trace_method]` too).

### 2.5 Expose a drain endpoint

```rust
#[query]
fn __debug_drain() -> Vec<u8> { ic_debug_trace::drain() }
```

The agent will call this after each traced flow and POST the CBOR blob
to `ic-debug record`.

### 2.6 Drive it from Node

```js
import { newTrace } from "ic-debug-agent-js";

const trace = await newTrace("http://127.0.0.1:9191", "my flow");
await myCanister.do_thing(trace.header(), …args);
await drainAllCanisters();  // see examples/scripts/ for the pattern
```

---

## 3. The CLI

One binary, four subcommands.

### `ic-debug record`

Runs the recorder daemon. Writes everything to a single SQLite file.

```bash
ic-debug record [--store <path>] [--port 9191]
```

Endpoints:

| Route | Purpose |
|---|---|
| `POST /traces` | agent registers a trace (body: `{trace_id, label}`) |
| `POST /events` | agent-side events (ingress, notes) |
| `POST /drain`  | canister CBOR drain blob |

### `ic-debug replay`

Renders a trace as a causally-ordered timeline in the terminal. Cross-
canister calls are spliced at the `CallSpawned` point so the output
reads top-to-bottom the way a call tree should.

```bash
ic-debug replay --trace <uuid> [--store <path>] [--step] [--json] [--decode-cbor]
```

- `--step` — pause after each event; press enter to advance.
- `--json` — emit the entire trace as a single JSON doc (pipe to `jq`).
- `--decode-cbor` — inline-decode `StateSnapshot` payloads (on by default).

Example:

```
#003 t63gs seq=2 parent=1 ● STATE  payment         {"id":1,"amount":100,"status":"Pending"}
#004 t63gs seq=3 parent=2 → CALL   lock_funds on tz2ag
#005 tz2ag seq=0 parent=3 ▶ ENTER  lock_funds      caller=t63gs-up777-…
```

### `ic-debug diff`

Walks state snapshots and prints transitions as JSON-pointer-style
deltas. Two modes:

```bash
# default: every (canister,key) pair, every transition
ic-debug diff --trace <uuid>

# specific pair
ic-debug diff --trace <uuid> --canister t63gs-…-cai --key payment --from 2 --to 6

# machine-readable
ic-debug diff --trace <uuid> --json | jq
```

Delta shapes:

- `{"Added":   {"path":"/foo","value":"…"}}` — new field / element
- `{"Removed": {"path":"/foo","value":"…"}}`
- `{"Changed": {"path":"/status","from":"\"Pending\"","to":"\"Locked\""}}`

### `ic-debug serve`

Read-only HTTP layer over a trace store, plus the web UI.

```bash
ic-debug serve [--store <path>] [--port 9192] [--ui-dir ui/dist]
```

| Route | Returns |
|---|---|
| `GET /health` | `ok` |
| `GET /api/traces` | list of registered traces + event counts |
| `GET /api/traces/:id` | `{summary, events[]}` (same layout as `replay --json`) |
| `GET /api/traces/:id/diff` | `{transitions[], initials[]}` |
| `GET /api/traces/:id/snapshot/:canister/:seq/:key` | raw decoded CBOR snapshot |

Any path not starting with `/api` or `/health` falls through to the
static UI bundle — so this is also what you browse from Chrome.

---

## 4. The web UI

Layout (desktop, ≥ ~1200 px wide):

```
┌─────────────┬─────────────────────────────────────┬──────────────────┐
│  traces     │  timeline                           │  event detail    │
│             │                                     │                  │
│  23de17…    │  #001 t63gs seq=23      ▶ ENTER …   │  canister …      │
│  happy      │  #002 t63gs seq=24      · NOTE  …   │  seq …           │
│  failed     │  #003 t63gs seq=25      ● STATE …   │  raw JSON        │
│             │  #004 t63gs seq=26      → CALL  …   │                  │
│  11223344   │  #005 tz2ag seq=8       ▶ ENTER …   │  ────────────    │
│             │  ▼ …                                │  state up to     │
│             │                                     │  cursor          │
│             │                                     │  payment:        │
│             │                                     │   Pending→Locked │
└─────────────┴─────────────────────────────────────┴──────────────────┘
```

- **Trace list** — click to load. Each row shows id prefix, event count, and the label you passed to `newTrace(…, label)`.
- **Timeline** — one row per event, causally ordered. Canister tag is colored deterministically from the principal hash; `seq` is per-canister; `+n` is the parent seq link. The glyph column encodes the event kind (see §5). Click any row to jump the cursor there.
- **Event detail** — for the cursor event, all the metadata and the raw JSON payload (including CBOR-decoded `StateSnapshot` bodies).
- **State up to cursor** — initials and fired transitions, filtered by whether the cursor has passed their `to_seq`. Rewind the cursor and the state panel rewinds with it. Changed fields render as red `from` → green `to`; added fields green `+`; removed red `−`.

### Error flagging

The UI automatically flags events that look like errors and highlights
them without requiring any extra instrumentation. An event is flagged if:

| Condition | Example |
|---|---|
| `method_exited` with a non-null `reject` field | canister panicked or trapped |
| `call_returned` with a non-null `reject` field | inter-canister call rejected at the IC level |
| `note` whose label matches `/fail\|reject\|rollback/i` | `trace_event!("submit_payment:rollback_missing")` |

> **Note on Candid errors:** a `Result::Err` returned by a callee is
> *not* an IC-level reject — it's a successful reply with an `Err`
> variant in the payload. To flag that case, emit an explicit note with a
> matching label: `trace_event!("send_receipt:rejecting")`.
>
> **Blind spot — no notes at all:** if the failing branch emits no
> `trace_event!`, the UI has nothing to flag. The trace looks completely
> clean (`◀ EXIT`, no ⚠ pill) even though the call did nothing useful.
> See `examples/scripts/04-silent-error.mjs` for a concrete case
> (`escrow.release` on a non-existent lock). The fix is always the same:
> add a note whose label contains "fail", "reject", or "rollback" in the
> branch that represents an error condition.

What you see when there are flagged events:

- **Header pill** — `⚠ N errors` badge appears next to the event count.
- **Row tinting** — flagged rows are tinted red with a red left border and a `⚠` glyph in the gutter.
- **Detail banner** — when the cursor is on a flagged event, the event-detail panel shows a red `⚠ flagged: <reason>` banner at the top.
- **`n` key** — jumps forward to the next flagged event, wrapping at the end.

### Reading the failure trace (script 03)

1. Load the **`03-flag-an-error`** trace.
2. Press **End** — final event is `EXIT` on `submit_payment` with no reject.
3. The state panel shows `payment.status: "Pending" → "Locked"` — one transition, never a second.
4. Press **Home**, then step with **→** until cursor hits the `NOTE submit_payment:rollback_missing` event. That's the bug report the canister itself emitted.
5. Switch to **`02-multi-canister`** for comparison — same flow shape up to that point, then a second `payment` transition `"Locked" → "Completed"` plus `lock.released: false → true`.

---

## 5. The event model

Every event row has the same envelope:

```ts
{ idx, canister, seq, parent_seq, span_id, ts_nanos, kind }
```

- `idx` — position in the *causally-ordered* replay (monotonic, gapless).
- `canister` — principal text of the canister that emitted the event (or `null` for agent-side events).
- `seq` — **per-canister** monotonic counter. Resets per canister, not per trace.
- `parent_seq` — seq of the event that caused this one. Cross-canister links are how the replay splicer reconstructs call trees.
- `span_id` — local span identifier inside a canister, set by `#[trace_method]`.
- `kind` — a tagged union:

| `kind` | Shape | Emitted by |
|---|---|---|
| `method_entered` | `{method, caller}` | `#[trace_method]` entry |
| `ingress_entered` | `{method, caller, args_hash}` | agent-side (optional) |
| `method_exited` | `{reject: null \| string}` | `#[trace_method]` exit |
| `call_spawned` | `{target, method, args_hash}` | `call_traced!` |
| `call_returned` | `{reject: null \| string}` | `call_traced!` on completion |
| `state_snapshot` | `{key, cbor: bytes}` | `trace_state!` |
| `note` | `{label}` | `trace_event!` |
| `timer_fired` | `{label}` | ic-cdk timer shims (planned) |

> Note: a Candid `Result::Err` returned by a callee is **not** a `reject`.
> It's a successful reply with an `Err` variant inside the payload. The
> reference demo is built around this distinction — you need
> `trace_event!` notes to surface it.

---

## 6. Keyboard shortcuts

| Key | Action |
|---|---|
| `→` / `j` | Step forward one visible event |
| `←` / `k` | Step backward one visible event |
| `↑` | Step backward one visible event |
| `↓` | Step forward one visible event |
| `Home` | Jump to first event |
| `End` | Jump to last event |
| `Space` | Toggle autoplay (500 ms/tick; stops at last event) |
| `n` | Jump to next flagged (error) event; wraps around |
| `/` | Open inline search (filters timeline rows by text) |
| `Escape` | Close search / dismiss |

Click anywhere in the timeline list to move the cursor to that event.

Hidden canisters (eye icon in the header) and collapsed call subtrees
(click the `▶` glyph) are skipped by all keyboard navigation.

---

## 7. Troubleshooting

### "no trace selected" on first load

`GET /api/traces` returned an empty list — you haven't drained anything
yet. Run a flow (e.g. `node examples/scripts/01-hello-trace.mjs`).

### Every event has `parent_seq` = 0 and no call links appear

The caller wasn't using `call_traced!` (or didn't have
`#[trace_method]` on the callee). Parent links are *only* set when the
callee receives a `TraceHeader` first arg and the macro promotes it to a
live trace context.

### "current_trace=None" in the recorder logs

The macro ordering is wrong: `begin_trace(header)` must run before any
other ic-debug-trace call on this method. If you hand-rolled
instrumentation instead of using `#[trace_method]`, make sure the
`begin_trace` is the *first* line.

### Canister buffer keeps growing

`__debug_drain` empties it. Call the drain after every logical flow
(the example scripts and `examples/collect.mjs` both do this).

### Port conflicts

Everything is configurable — `--store`, `--port`, `--ui-dir`. The
recorder defaults to `9191` and serve defaults to `9192`; nothing in
the code cares, they just have to match what the example scripts (or
your own driver) are POSTing to via `RECORDER_URL`.
