# Stress test findings

Five synthetic traces and two source fixtures, designed to exercise every
documented rule of `ic-debug instrument` and every layer of the debugger
(record/replay/diff/serve). Run from the repo root:

```bash
# Wizard fixtures
target/release/ic-debug instrument test-fixtures/stress-wizard/stress_rust.rs --dry-run
target/release/ic-debug instrument test-fixtures/stress-wizard/stress_motoko.mo --dry-run
target/release/ic-debug instrument test-fixtures/stress-wizard/stress_motoko_bootstrap.mo --dry-run

# Debugger traces
"C:/Python312/python.exe" test-fixtures/stress-debugger/stress_traces.py
target/release/ic-debug serve --store test-fixtures/stress-debugger/stress_traces.sqlite --port 9195
```

---

## Wizard — Rust side

`stress_rust.rs` provokes every rule plus their negative-guard cases. Output:
**16 candidates** vs. expected 18; one over-fire was actually intentional.

### Bug 1 — Rule 1b silently skips `#[init]` and `#[post_upgrade]`

GUIDE.md and the function `attr_is_cdk_write_entry` both list
`init`/`post_upgrade` as Rule 1b targets, but the upstream check
`attr_is_cdk_entry` (in [detect.rs:30](crates/ic-debug-cli/src/instrument/detect.rs:30))
only recognises `update / query / heartbeat / inspect_message`:

```rust
const CDK_ENTRY_ATTRS: &[&str] = &["update", "query", "heartbeat", "inspect_message"];
```

`scan_fn` finds the CDK attribute by `attr_is_cdk_entry` first, so `#[init]`
never reaches Rule 1b. The fixture's `rule_1b_init` confirms: no candidate
fires on it. **Fix**: add `init` and `post_upgrade` to `CDK_ENTRY_ATTRS`
(and `CDK_ENTRY_PATHS` for the qualified spellings).

### Bug 2 — Rule 5 (rollback-note) doesn't fire on the common `if cond { return Err }` shape

In [detect.rs:294](crates/ic-debug-cli/src/instrument/detect.rs:294) the
`state_traced_in_block` flag is local to a `scan_block` invocation. When
the wizard recurses into a nested block (e.g. an `if` body), the flag
resets, so a `return Err(...)` inside `if !ok { ... }` doesn't see the
function-level `trace_state!` that was emitted earlier.

Real-world early-return rollbacks are almost always inside an `if` or
`match` arm. The fixture confirms: `rule_5_rollback` and the early-return
inside `combined_method` both have `trace_state!` followed by
`if !ok { return Err(...) }`, and neither produces a candidate. **Fix**:
thread an "ancestor-block already had a trace_state" flag down through
`scan_block` recursion. (Watch out for closure/async boundaries — those
should still cut the chain.)

### Cascade behaviour — by design but worth noting in the guide

Rule 3 (entry-note) fires alongside Rule 1 (wrap-method) on un-wrapped
methods, e.g. `rule_1_wrap` in the fixture. The comment in
[detect.rs:131-138](crates/ic-debug-cli/src/instrument/detect.rs:131)
says this is intentional ("Rule 1 / 1b just suggested adding it" cascades
into body rules). Documented: this saves a second wizard pass. Not
documented: the same cascade fires Rules 4/6/7 on un-wrapped methods —
my expected-count was off by 6 because I assumed body rules wait for the
wrap to land. Worth a sentence in GUIDE.md §2.0.

### Verified correct (no false positives, no false negatives)

| Rule | Positive cases | Negative cases that correctly stayed silent |
|---|---|---|
| 1 | `rule_1_wrap`, `combined_method` | — |
| 1b | `rule_1b_insert_header` | `neg_query_excluded` (`#[query]`), `already_instrumented` |
| 2 | `rule_2_call`, `combined_method` | `rule_2_negative` (in string + comment + helper wrapper) |
| 3 | `rule_3_entry_note`, plus cascades | `already_instrumented` |
| 4 | `rule_4_snapshot_local`, `combined_method` | — |
| 6 | `rule_6_trap`, `rule_6_trap_api`, `combined_method` | — |
| 7 | `rule_7_mutation` (×2: COUNTER + USERS), `combined_method` | `rule_7_negative_unknown` (UNKNOWN_STATE not `thread_local!`) |

---

## Wizard — Motoko side

`stress_motoko.mo` (M2..M7 in one file) + `stress_motoko_bootstrap.mo` (M1).
**9 candidates** in stress_motoko.mo, **1** in bootstrap, **zero false
positives**, **zero misses**.

### Verified correct

| Rule | Positive | Negatives correctly skipped |
|---|---|---|
| M1 | `Bootstrap` actor (no Trace import / tracer / drain) | — |
| M2 | `m2_wrap_method` | already-wrapped methods |
| M3 | `m3_insert_header`, plus `m5_negative_not_traced` and `m6_negative_not_traced` (which are independently public methods needing a header) | `neg_query_excluded` (query), `__debug_drain` (excluded by name) |
| M4 | `m4_entry_note` | methods that already have a `:enter` note |
| M5 | `m5_trap` | `m5_negative_not_traced` (no `tracer.beginTrace(` — not in a traced body) |
| M6 | `m6_rollback` | `m6_negative_string` (throw inside `"…"`), `m6_negative_already_noted` (`:rollback` on prev line), `m6_negative_not_traced` |
| M7 | `m7_mutation` (×2: balance + owner) | `m7_negative_record` (`r.field :=`), `m7_negative_array` (`arr[0] :=`), `m7_negative_shadow` (local var shadow), `m7_negative_unknown` (not an actor var), `m7_negative_already_snapshotted` (`snapshotText` already follows) |

The Motoko skip-mask + brace-depth + word-boundary guards are holding.
The shadow check (`body_declares_var_before`) correctly rejects the local
`var balance : Nat = 99` shadowing the actor-level `balance`.

### Convergence note — also worth documenting

M3 needs the tracer field to be present, so on a fresh actor M1 fires
first and only M1 — re-running after applying M1 then surfaces M3 on the
public methods. The bootstrap fixture confirms: dry-run shows only the
M1 candidate, not the (correct) M3 candidate that *would* fire on `bump`
once M1 lands. This is intentional convergence, but a one-liner in
GUIDE.md ("re-run the wizard after accepting M1 to see M3 candidates")
would help users.

---

## Debugger — replay / diff / serve

Five stress traces, **557 events / 252 snapshots / 5 traces** total.

| Trace | Events | Calls | Rejects | Canisters | Probes |
|---|---:|---:|---:|---:|---|
| deep_chain (5-level chain) | 24 | 4 | 0 | 5 | replay ✓, diff ✓, summary ✓, snapshot fetch ✓ |
| fan_out (12-way) | 63 | 12 | 0 | 2 | replay ✓, parent_seq links resolve ✓ |
| high_volume (200 iters) | 403 | 0 | 0 | 1 | replay ✓, diff ✓ (199 transitions), JSON ~123 KB OK |
| mixed_rejects | 14 | 2 | 4 | 3 | replay flags all 4 rejects ✓ |
| many_keys (25 keys × 2 snaps) | 53 | 0 | 0 | 1 | diff ✓ (25 transitions, 0 → 0 value-equal correctly suppressed) |

### Verified correct

- **Cross-canister parent_seq propagation** — deep_chain's stack unwinds
  cleanly: E exits → D's call_returned → D exits → C's call_returned →
  ... → A exits, with every parent_seq pointing at the correct
  call_spawned in the parent canister.
- **Replay splicer** — fan_out interleaves 12 outbound calls correctly,
  every B's `method_entered` lands directly after A's `call_spawned` and
  every B's `method_exited` lands directly before A's `call_returned`.
- **CBOR snapshot decode** — the `/snapshot/:can/:seq/:key` endpoint
  decodes both small ({"depth":1,…}, 27 bytes) and varied-shape
  ({"counter":i,"history":[…]}) payloads.
- **Diff engine** — Changed/Added deltas computed correctly; `key_00`'s
  `value: 0 → 0` (no change) is suppressed while `label: "before" →
  "after"` is reported.
- **Reject counter** — mixed_rejects's summary shows `rejects: 4`
  matching the four structured rejects (2 `method_exited` + 2
  `call_returned`); the `:rollback_missing` *note* is intentionally not
  in this counter (UI's regex flagger surfaces it separately).
- **API surface** — all five trace ids respond on `/api/traces`,
  `/api/traces/:id`, `/api/traces/:id/diff`, and
  `/api/traces/:id/snapshot/:can/:seq/:key` without errors.

### No bugs found in the debugger layer

The mojibake-looking `â†’` I observed in initial probing
was a Python `json.tool` artifact on Windows (cp1252 stdin decoding of
UTF-8 bytes). The raw Rust API output for the deep_chain label is
correct UTF-8 (`e2 86 92` = `→`).

---

## Summary

**Wizard**: 2 real bugs found, both Rust-side and pre-dating this
session's Motoko work. Motoko side (M1..M7) is clean — zero false
positives across 11 deliberate negative cases.

**Debugger**: passes every shape thrown at it (depth-5 cross-canister
chain, 12-way fan-out, 400-event volume, mixed reject types, 25 distinct
state keys). No bugs.

### Suggested follow-ups

1. **Fix Rule 1b coverage**: add `init` / `post_upgrade` to `CDK_ENTRY_ATTRS`.
2. **Broaden Rule 5 scope**: track `state_traced_in_block` across
   nested-block boundaries so `if cond { return Err(...) }` triggers
   rollback-note (the most common shape).
3. **Document cascade + convergence behaviour** in GUIDE.md §2.0:
   - Rust: Rule 1 cascades into body rules in the same pass.
   - Motoko: M1 / M3 are convergence-staged; users may need a second run.
