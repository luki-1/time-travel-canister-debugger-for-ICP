# ic-debug — User Guide

A time-travel debugger for ICP canisters. Record every inter-canister call,
state snapshot, and rejection from a payment flow (or your own), then walk
through the causal timeline event-by-event and watch state evolve.

This guide walks you through:

1. [Running the reference demo](#1-running-the-reference-demo)
2. [Instrumenting your own canisters](#2-instrumenting-your-own-canisters)
   - [Bootstrap with `ic-debug instrument` (the wizard)](#20-bootstrap-with-ic-debug-instrument-the-wizard)
3. [The CLI: `record` / `replay` / `diff` / `serve` / `instrument`](#3-the-cli)
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
icp network start
icp deploy --mode reinstall
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

### Run all five scenarios

```bash
node agent-js/demo.mjs
```

This executes five back-to-back flows against the three sample canisters
and records each as a separately-labelled trace. Open
**http://127.0.0.1:9192** and you'll see all five in the left rail.

| # | Label | What it demonstrates |
|---|---|---|
| 1 | `demo: happy path` | Baseline. `submit_payment(100)` succeeds end-to-end. Payment reaches `Completed`. Use this as the reference shape. |
| 2 | `demo: failed notifications` | Armed `notifications.arm_failure()` + a buggy `Err` branch in `frontend_api`. Payment strands at `Locked`. Note `submit_payment:rollback_missing` is the smoking gun. |
| 3 | `demo: double payment` | Two `submit_payment` calls in the **same** trace (same trace id, two root spans). Exposes the "shared key, multiple entities" pitfall: `trace_state!("payment", …)` with two different payments in flight produces weird "Completed → Pending" backward-looking transitions in the diff panel — the id changes under the same key. Debugging insight: key snapshots per logical entity, e.g. `format!("payment/{}", id)`. |
| 4 | `demo: release after payment` | Happy `submit_payment` followed by an explicit `escrow.release(id)`. Captures the `lock.released: false → true` transition the other flows never reach. |
| 5 | `demo: recover and release` | The full remediation arc: arm failure → payment #1 strands `Locked` → payment #2 runs happy → operator calls `escrow.release(#1)` to free the stranded funds. Final state is deliberately **inconsistent across canisters**: `payment[#1].status = Locked` in frontend_api, but `lock[#1].released = true` in escrow. The debugger is the only tool that makes the disagreement visible. |

### Reading the smoking-gun trace (scenario #2)

The `frontend_api` canister has an intentional bug: if
`notifications.send_receipt` returns `Err(…)` (or the call rejects), the
`submit_payment` method records a `rollback_missing` note and returns
without rolling back the escrow lock. Standard IC tooling sees a
*successful* call (Candid `Result::Err` is not an IC-level reject), so the
bug is easy to miss in production.

The debugger makes it impossible to miss:

| Artefact | Happy path | Failure path |
|-----------|---|---|
| Final `payment.status` | `Completed` | **`Locked`** ← never cleared |
| State transitions | `Pending → Locked → Completed` | `Pending → Locked` |
| Escrow `lock.released` | `true` (via scenario #4 or #5) | **`false`** ← funds stranded |
| Diagnostic note | — | `submit_payment:rollback_missing` |

The right-hand state panel renders the diff live as you step. See also
[§4](#4-the-web-ui).

### Reading the cross-canister inconsistency (scenario #5)

Jump to the end of `demo: recover and release` (press `End`). The state
panel shows two `payment` transitions and one `lock` transition —
finishing with `lock.released: false → true` on payment #9. But
`payment[#9].status` is still `Locked` in `frontend_api` because the
release was called directly against escrow, not routed through
`frontend_api`. Two canisters now disagree about the same logical
payment. That disagreement is invisible in standard logs; in the
timeline + state panel it's two clicks away.

---

## 2. Instrumenting your own canisters

The fast path is the wizard (section 2.0); the slower path is doing the
same edits by hand (sections 2.1 onwards). The wizard's output is plain
Rust source — exactly what you'd write yourself — so reading 2.1–2.6
remains the right way to understand *what* the instrumentation does.

### 2.0 Bootstrap with `ic-debug instrument` (the wizard)

`ic-debug instrument <path>` walks Rust (`.rs`) or Motoko (`.mo`)
source — one file or a whole directory — and asks, for each candidate
site, whether to insert a tracing call. The output is plain source the
wizard rewrites into the file in place. Detection is purely syntactic
and held to a **zero false positives** bar — every prompt corresponds
to an edit that produces valid code.

```bash
# Walk one file. The wizard prints each candidate with surrounding
# context and asks accept / decline / skip-rest / quit.
ic-debug instrument canisters/escrow/src/lib.rs

# Walk a whole directory. Every .rs and .mo file is processed
# independently; build dirs (target/, node_modules/, dist/, build/,
# hidden dirs) are skipped automatically.
ic-debug instrument canisters/

# List candidates without writing or prompting (good first step).
ic-debug instrument canisters/escrow/src/lib.rs --dry-run

# Emit a unified diff of all candidates to stdout — useful for code
# review or scripted CI checks. No file is modified.
ic-debug instrument canisters/escrow/src/lib.rs --diff-only

# Accept everything non-interactively (Rule 4 / Rule 7 fall back to
# default keys). Use only when you trust the rule set.
ic-debug instrument canisters/escrow/src/lib.rs --apply-all
```

#### What the wizard suggests

Seven rules. Each fires only when its preconditions prove the edit is
correct; if it can't prove that, it stays silent.

| # | Rule | Fires on | Insertion |
|---|---|---|---|
| **1**  | wrap-method                | `#[update]` / `#[query]` / `#[heartbeat]` / `#[inspect_message]` whose first param is already `TraceHeader` | `#[trace_method]` above the existing CDK attribute |
| **1b** | wrap-method-insert-header  | `#[update]` / `#[init]` / `#[post_upgrade]` / `#[heartbeat]` / `#[inspect_message]` with **no** `TraceHeader` (queries excluded — see notes) | inserts both `#[trace_method]` *and* `header: TraceHeader,` as the first parameter — **breaking ABI change**, callers must update |
| **2**  | convert-call               | a literal `ic_cdk::call(target, "method", (args,))` (or local alias) inside a `#[trace_method]` body | rewrites it as `call_traced!(target, "method", (args,))`. **Caveat:** the callee must accept `TraceHeader` — the wizard cannot verify that |
| **3**  | entry-note                 | `#[trace_method]` body whose first statement is not a `trace_event!(...:enter)` | inserts `trace_event!("<fn_name>:enter");` as the first statement |
| **4**  | snapshot-local             | `let <ident> = SomeStruct { ... };` inside a `#[trace_method]` body | prompts for a key, then inserts `trace_state!("<key>", &<ident>);` |
| **5**  | rollback-note              | a literal `return Err(...);` in a `Result`-returning `#[trace_method]`, **and** a `trace_state!` was already emitted earlier in the same block | inserts `trace_event!("<fn_name>:rollback");` on the line above |
| **6**  | trap-note                  | a call to `ic_cdk::trap` or `ic_cdk::api::trap` inside a `#[trace_method]` body | inserts `trace_event!("<fn_name>:trapped");` on the line above |
| **7**  | mutation-snapshot          | `<NAME>.with(\|x\| ... x.borrow_mut() ...)` inside a `#[trace_method]` body, where `<NAME>` is a `thread_local!` declared in the same file | prompts for key + value, then inserts `trace_state!(...)` after the `.with()` call |

#### What the wizard suggests for Motoko (`.mo` files)

Five rules — the Motoko `Trace` library has no macros, so each rule's
edit is a few explicit `tracer.<method>(...)` lines instead of a
single attribute. The wizard converges in three passes: bootstrap →
wrap-method → notes.

| # | Rule | Fires on | Insertion |
|---|---|---|---|
| **M1** | mo-bootstrap                | actor with no `tracer = Trace.Tracer(...)` field | inserts (whichever are missing) `import Trace`, the `transient let tracer = …` field, and a `public query func __debug_drain() : async Blob { tracer.drain() };` — all in one edit |
| **M2** | mo-wrap-method              | `public … func <fn>(header : ?Trace.TraceHeader, …) : …` whose body contains no `tracer.beginTrace(` | inserts `tracer.beginTrace(header) / methodEntered(...) / methodExited(null)` boilerplate, placing `methodExited` *before* any trailing return expression |
| **M3** | mo-wrap-method-insert-header | `public ` write `func <fn>(...)` with no `?Trace.TraceHeader` first param (queries excluded; `__debug_drain` excluded) | also splices `header : ?Trace.TraceHeader` into the parameter list — **breaking ABI change**, agent-js callers re-scanned afterwards |
| **M4** | mo-entry-note               | function body has `tracer.methodEntered(...)` but no `tracer.note("…:enter")` near the top | inserts `tracer.note("<fn>:enter");` right after `methodEntered(...)` |
| **M5** | mo-trap-note                | `Debug.trap(...)` inside a body that already has `tracer.beginTrace(`, with no `:trapped` note on the previous line | inserts `tracer.note("<fn>:trapped");` on the line above the trap |
| **M6** | mo-rollback-note            | `throw` expression inside a body that already has `tracer.beginTrace(`, with no `:rollback` note on the previous line | inserts `tracer.note("<fn>:rollback");` on the line above the throw |
| **M7** | mo-mutation-snapshot        | actor-level `var x :=` assignment inside a traced body, where `x` is declared at actor scope (not a local shadow), with no `snapshotText`/`snapshotBlob` in the next 5 lines — bare ident LHS only (no `r.field :=` or `arr[i] :=`) | prompts for a key, then inserts `tracer.snapshotText("<key>", debug_show x);` after the assignment |

The rules also cooperate with the Rust agent-js post-step: when an
**M3** candidate is accepted, the wizard offers to update matching
`<actor>.<method>(...)` and `<method>: IDL.Func([...])` patterns under
`agent-js/` so JS callers stay in sync, the same as for Rust Rule 1b.

##### Path resolution for `import Trace`

The bootstrap rule needs to know the relative path from the canister
file to `motoko/src/Trace.mo`. The wizard walks the directory tree
upwards from the canister, looking for that file as a sibling. If
found, it emits e.g. `import Trace "../../../motoko/src/Trace";`. If
nothing is found within ten levels, it emits a placeholder with a
`/* TODO: fix import path */` comment so the diff makes the failure
obvious.

##### Limits

- The wizard never converts inter-canister calls. Motoko `actor`
  references look like ordinary method calls (`escrowActor.lock_funds(…)`),
  and there's no syntactic difference between an instrumented call and
  a non-instrumented one — the user inserts `tracer.callSpawned(…)` /
  `tracer.callReturned(…)` by hand.
- Rule M7 (mutation-snapshot) only fires on bare-ident LHS assignments
  (`x := expr`). Record field updates (`r.field := expr`) and array
  element updates (`arr[i] := expr`) are silently skipped. It also only
  fires when the var is declared at actor scope in the same file — cross-
  file state and `stable var` fields in separate modules are out of scope.
- The wizard assumes a tracer alias of `Trace` for the bootstrap
  insertion. If you import the module under a different alias, the
  wizard recognises it on subsequent runs (it picks up whatever
  `<alias>.Tracer(` you've used) but bootstrap itself always emits
  `import Trace "..."`.

#### What it deliberately does **not** detect

These would risk false positives, so the wizard stays silent and you
add the call by hand.

- **Generated Candid stubs** like `actor.method(args).await`. They look
  like any other JS-style method call; we can't tell them apart from
  unrelated chained APIs without type information.
- **Helper-wrapped calls.** `helpers::send(...)` that internally calls
  `ic_cdk::call` is invisible — only the literal `ic_cdk::call(...)`
  expression matches Rule 2.
- **Method calls inside closures** passed to `tokio::spawn` or `async`
  blocks. The wizard skips those bodies because emitting `trace_event!`
  there would fire at unexpected times.
- **`STATE.with(...)` patterns where `STATE` is *not* a `thread_local!`
  in the same file.** Cross-file state containers and stable-structures
  variants are out of Rule 7's scope.
- **The `?` operator as a rollback trigger.** Too noisy — `?` exits on
  every error in any subexpression. Rule 5 only fires on literal
  `return Err(...)`.
- **Adding `TraceHeader` to `#[query]` methods.** Queries are read-only
  and rarely worth a breaking signature change; if you really want to
  trace one, do it by hand.

#### The agent-js post-step

When Rule 1b accepts (TraceHeader insertion), the wizard automatically
scans `agent-js/` (configurable with `--agent-js-root <PATH>`, opt out
with `--skip-agent-js`) for two patterns:

- `<method>: IDL.Func([...])` definitions — splices `Header,` into the
  argument list.
- `<receiver>.<method>(...)` call sites — splices `trace.header(),`
  into the argument list.

Both patterns are conservative: substring matches (`lock_funds_v2` won't
match `lock_funds`) and idempotent (already-updated lines are skipped).
Each match is shown to the user before edits are written.

#### Worked example

You have a fresh canister method that's not instrumented:

```rust
#[update]
fn lock_funds(payment_id: u64, amount: u64) -> u64 {
    payment_id + amount
}
```

Run the wizard:

```bash
ic-debug instrument canisters/escrow/src/lib.rs
```

The wizard finds Rule 1b (no `TraceHeader`, no `#[trace_method]`) and
warns "BREAKING API CHANGE: every caller must be updated." Accept it.
The file is rewritten:

```rust
#[trace_method]
#[update]
fn lock_funds(header: TraceHeader, payment_id: u64, amount: u64) -> u64 {
    payment_id + amount
}
```

The agent-js post-step then scans `agent-js/` for the affected method
and prompts to update the IDL stub and call sites. Accept it; the
script files are rewritten with `Header,` and `trace.header(),`
prepended.

Run the wizard a second time. With `#[trace_method]` now in place,
Rule 3 fires — the body lacks an entry note. Accept it:

```rust
#[trace_method]
#[update]
fn lock_funds(header: TraceHeader, payment_id: u64, amount: u64) -> u64 {
    trace_event!("lock_funds:enter");
    payment_id + amount
}
```

Run a third time: zero candidates. Idempotent.

Sections 2.1–2.6 below describe the same edits manually — read them
to understand *what* the wizard generates and *why*.

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
await drainAllCanisters();  // see agent-js/demo.mjs for the pattern
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

### `ic-debug instrument`

Interactive setup wizard. Walks Rust *and* Motoko source for canister
code and inserts the appropriate trace calls in place — `#[trace_method]`,
`call_traced!`, `trace_event!`, `trace_state!` for `.rs`; the
`tracer.beginTrace` / `methodEntered` / `methodExited` boilerplate plus
`Trace.Tracer(...)` field and `__debug_drain` query for `.mo`. Detection
is held to a zero false-positive bar; see
[section 2.0](#20-bootstrap-with-ic-debug-instrument-the-wizard) for the
full rule list and the limits.

```bash
ic-debug instrument <path>                            # interactive on file or directory
                    [--dry-run]                       # list candidates, don't prompt or write
                    [--diff-only]                     # write a unified diff to stdout
                    [--apply-all]                     # accept everything, no prompts
                    [--agent-js-root <DIR>]           # default: agent-js
                    [--skip-agent-js]                 # disable the post-step
```

Behaviour notes:

- `<path>` is a `.rs` or `.mo` file *or* a directory. Directories are
  walked recursively; `target/`, `node_modules/`, `dist/`, `build/`,
  and hidden folders are skipped.
- The interactive flow prompts for each candidate and shows ~3 lines
  of source context. Rules 4 and 7 ask additionally for a snapshot
  key (and Rule 7 for a value expression).
- Re-runs are idempotent: every rule has a precondition that checks
  for the absence of its own suggested edit.
- The agent-js post-step only runs after a Rule 1b candidate is
  accepted (TraceHeader insertion) and only if `--agent-js-root`
  exists. It rewrites IDL.Func definitions and `<actor>.<method>(...)`
  call sites to thread the header through.

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
> See `agent-js/examples/04-silent-error.mjs` for a concrete case
> (`escrow.release` on a non-existent lock). The fix is always the same:
> add a note whose label contains "fail", "reject", or "rollback" in the
> branch that represents an error condition.

What you see when there are flagged events:

- **Header pill** — `⚠ N errors` badge appears next to the event count.
- **Row tinting** — flagged rows are tinted red with a red left border and a `⚠` glyph in the gutter.
- **Detail banner** — when the cursor is on a flagged event, the event-detail panel shows a red `⚠ flagged: <reason>` banner at the top.
- **`n` key** — jumps forward to the next flagged event, wrapping at the end.

### Reading the demo's failure trace

1. Load the **`demo: failed notifications`** trace.
2. Press **End** — final event is `EXIT` on `submit_payment` with no reject.
3. The state panel shows `payment.status: "Pending" → "Locked"` — one transition, never a second.
4. Press **Home**, then step with **→** until cursor hits `#018 NOTE submit_payment:rollback_missing`. That's the bug report the canister itself emitted.
5. Switch to **`demo: happy path`** for comparison — you'll see the same flow up to event 17, then a second `payment` transition `"Locked" → "Completed"` plus an eventual `lock.released: false → true`.

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
| `→` / `j` | Step forward one event |
| `←` / `k` | Step backward one event |
| `Home` | Jump to event #1 |
| `End` | Jump to last event |
| `Space` | Toggle autoplay (500 ms/tick; stops at last event) |
| `n` | Jump to next flagged (error) event; wraps around |

Click anywhere in the timeline list to move the cursor to that event.

---

## 7. Troubleshooting

### "no trace selected" on first load

`GET /api/traces` returned an empty list — you haven't drained anything
yet. Run a flow (or re-run `node agent-js/demo.mjs`).

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
(the bundled `agent-js/demo.mjs` and `agent-js/collect.mjs` both do
this).

### Port conflicts

Everything is configurable — `--store`, `--port`, `--ui-dir`. The
recorder defaults to `9191` and serve defaults to `9192`; nothing in
the code cares, they just have to match what `agent-js/demo.mjs` (or
your own driver) is POSTing to via `RECORDER_URL`.
