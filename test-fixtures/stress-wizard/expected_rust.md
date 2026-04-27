# Expected wizard candidates for stress_rust.rs

Each line is `<rule>  <fn_name>  <why>`. Anything not on this list that fires
is a false positive; anything on this list that does not fire is a miss.

## Should fire (positives)

1. `wrap-method`                 `rule_1_wrap`                       — has TraceHeader, no `#[trace_method]`
2. `wrap-method-insert-header`   `rule_1b_insert_header`             — `#[update]` without TraceHeader
3. `wrap-method-insert-header`   `rule_1b_init`                      — `#[init]` without TraceHeader
4. `convert-call`                `rule_2_call`                       — literal `ic_cdk::call(...)` inside `#[trace_method]`
5. `entry-note`                  `rule_3_entry_note`                 — `#[trace_method]` body has no `:enter` event
6. `snapshot-local`              `rule_4_snapshot_local`             — `let payment = Payment { ... }`
7. `rollback-note`               `rule_5_rollback`                   — `return Err(...)` after `trace_state!`
8. `trap-note`                   `rule_6_trap`                       — `ic_cdk::trap(...)`
9. `trap-note`                   `rule_6_trap_api`                   — `ic_cdk::api::trap(...)`
10. `mutation-snapshot`          `rule_7_mutation`                   — `COUNTER.with(... borrow_mut() ...)`
11. `mutation-snapshot`          `rule_7_mutation`                   — `USERS.with(... borrow_mut() ...)`

## Combined method — every rule above except Rule 1b should fire here

12. `wrap-method`                `combined_method`                   — has TraceHeader
13. `convert-call`               `combined_method`                   — has `ic_cdk::call(...)`
14. `entry-note`                 `combined_method`                   — no `:enter` (after Rule 1 wraps)
15. `snapshot-local`             `combined_method`                   — `let payment = Payment { ... }`
16. `rollback-note`              `combined_method`                   — `return Err(...)` after snapshot
17. `trap-note`                  `combined_method`                   — `ic_cdk::trap(...)`
18. `mutation-snapshot`          `combined_method`                   — `COUNTER.with(...)`

## Must NOT fire (negatives — false-positive guards)

- `neg_query_excluded`              — `#[query]` is excluded from Rule 1b
- `already_instrumented`            — already has `#[trace_method]` and `:enter` note
- `rule_2_negative`                 — `ic_cdk::call` only appears in string + comment
- `rule_5_negative_question_mark`   — uses `?` operator, not `return Err`
- `rule_5_negative_no_state`        — `return Err` but no prior `trace_state!`
- `rule_7_negative_unknown`         — `UNKNOWN_STATE.with(...)` ident not declared as `thread_local!`

Note on Rule 3 expectations: when an `#[update]` has no `#[trace_method]`,
Rule 1 fires but Rule 3 does NOT (we check entry-note only on already-wrapped
methods). After accepting Rule 1, a *subsequent* run will surface Rule 3.
That's why the combined method's Rule 3 candidate (#14) appears here in the
expected list — it'll show up on the second pass, not the first.
