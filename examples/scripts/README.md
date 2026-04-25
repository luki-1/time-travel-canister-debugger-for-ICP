# ic-debug examples

Four short scripts, each with one job. Read them in order — later
examples assume you understand the earlier ones.

| File | What you learn |
|---|---|
| `01-hello-trace.mjs`   | Minimum code to record a single traced call (one canister, no fan-out). |
| `02-multi-canister.mjs` | How one trace follows a call across canisters. |
| `03-flag-an-error.mjs`  | What a failure looks like in the UI (flagged). |
| `04-silent-error.mjs`   | What a failure looks like when no notes are emitted (not flagged). |

## Prereqs

```bash
# from repo root
cargo build --release -p ic-debug-cli
npm --prefix agent-js install && npm --prefix agent-js run build
npm --prefix ui install        && npm --prefix ui run build
npm --prefix examples install   # @dfinity/agent + candid + principal

icp network start -d
# icp.yaml lives in examples/ — deploy from there
cd examples && icp deploy --mode reinstall && cd ..
```

## Run

In one terminal, start the recorder and UI host:

```bash
./target/release/ic-debug record --store traces/ic-debug.sqlite --port 9191 &
./target/release/ic-debug serve  --store traces/ic-debug.sqlite --port 9192 --ui-dir ui/dist &
```

Then run the examples (from the repo root — they read
`.icp/cache/mappings/local.ids.json`):

```bash
node examples/scripts/01-hello-trace.mjs
node examples/scripts/02-multi-canister.mjs
node examples/scripts/03-flag-an-error.mjs
node examples/scripts/04-silent-error.mjs
```

Each example prints the trace id and tells you what to look for in the UI
at http://127.0.0.1:9192.

## What each example is about

### 01 — Hello, trace

One canister, one traced method, one drain. The call is
`escrow.lock_funds(header, payment_id, amount)` — deliberately chosen
because it runs entirely inside escrow with no inter-canister fan-out,
so the resulting trace is four events long (enter → note → state
snapshot → exit). That's the full shape of a traced call with nothing
extra.

The only ic-debug API call is `newTrace(recorderUrl, label)`. Pass the
returned `header()` as the first arg to any instrumented canister
method and the canister's events are now tagged with this trace id.
After the call, POST the canister's `__debug_drain()` blob to the
recorder.

### 02 — Multi-canister

Same code as example 1, but `submit_payment` internally uses
`call_traced!` to fan out to escrow and notifications. One trace id,
three canisters, one timeline. The only difference from example 1 is
that you drain every participant, not just the ingress canister.

### 04 — Silent error

Calls `escrow.release` with a payment ID that was never locked. The
canister's `else` branch returns `None` and exits normally — no IC
reject, no `trace_event!`. The trace shows three clean events
(`method_entered`, a neutral note, `method_exited` with `◀`) and the UI
produces no ⚠ pill. The only signal that something went wrong is the
absence of a `STATE lock` snapshot that a successful release would have
emitted.

Contrast with example 03, where the canister emits a note that matches
the flagging pattern. Here it emits nothing, so the debugger is blind
unless the canister author adds a note with a label containing "fail",
"reject", or "rollback".

### 03 — Flag an error

Arms a reject in the notifications canister, then runs a normal traced
call. Demonstrates the UI's automatic error flagging: the trace header
shows an `⚠ N errors` pill, flagged events render in red with a `⚠`
glyph in the gutter, and the event-detail panel shows a reason banner.
Press `n` to cycle through flagged events.

The bug the trace captures is real: frontend_api's `submit_payment`
treats a `send_receipt` rejection as a no-op — the escrow lock stays
held. The state panel makes this visible (`payment.status` ends at
`Locked` instead of `Completed`).
