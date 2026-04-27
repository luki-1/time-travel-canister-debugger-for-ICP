# Expected wizard candidates for stress_motoko.mo + stress_motoko_bootstrap.mo

## stress_motoko_bootstrap.mo

1. `mo-bootstrap` — actor `Bootstrap` has no Trace import / tracer / drain
2. `mo-wrap-method-insert-header` — `bump` is public, no `?TraceHeader`

## stress_motoko.mo — should fire (positives)

1. `mo-wrap-method`                  `m2_wrap_method`        — has `?Trace.TraceHeader`, no `tracer.beginTrace(`
2. `mo-wrap-method-insert-header`    `m3_insert_header`      — public, no header param
3. `mo-entry-note`                   `m4_entry_note`         — has `methodEntered`, no `:enter` note
4. `mo-trap-note`                    `m5_trap`               — `Debug.trap(...)` in traced body
5. `mo-rollback-note`                `m6_rollback`           — `throw` in traced body, no `:rollback` above
6. `mo-mutation-snapshot`            `m7_mutation`           — `balance := …` (actor var)
7. `mo-mutation-snapshot`            `m7_mutation`           — `owner := …` (actor var)

## stress_motoko.mo — must NOT fire (negatives — false-positive guards)

- `neg_query_excluded`                  — query funcs excluded from M3
- `__debug_drain`                       — excluded from M3 (always)
- `m5_negative_not_traced`              — Debug.trap but no `tracer.beginTrace(` in body
- `m6_negative_string`                  — "throw" only appears inside string literal
- `m6_negative_already_noted`           — `:rollback` note already on the line above
- `m6_negative_not_traced`              — throw but no `tracer.beginTrace(` in body
- `m7_negative_record`                  — `record.field := …` (record-field update, not bare ident)
- `m7_negative_array`                   — `arr[0] := …` (array-element update)
- `m7_negative_shadow`                  — `balance := …` shadows local `var balance` declared earlier
- `m7_negative_unknown`                 — `local_only := …` not declared as actor var
- `m7_negative_already_snapshotted`     — `snapshotText` already follows the assignment

## Notes

- M2 fires *before* M4/M5/M6/M7 in a real workflow because un-wrapped
  methods don't have `tracer.beginTrace(` yet. We pre-wrap in this fixture
  so M4..M7 can be exercised in a single dry-run.
- `m6_negative_already_noted` — the wizard checks the *previous line* for
  a string ending in `:rollback`. If the precondition tightens later (e.g.
  scan more lines), this case still must not produce a duplicate note.
