//! One detection test per rule + idempotency. Each rule also has a
//! negative case that "almost matches" to lock in the zero-false-positive
//! contract against future regressions.

use super::detect;
use super::parse;
use super::Rule;

fn detect_rules(src: &str) -> Vec<Rule> {
    let file = syn::parse_file(src).expect("parse");
    let aliases = parse::collect_aliases(&file);
    detect::all(&file, src, &aliases).into_iter().map(|c| c.rule).collect()
}

// ---------- Rule 1: WrapMethod ----------

#[test]
fn rule_1_fires_when_cdk_attr_and_trace_header_present() {
    let src = r#"
use ic_debug_core::TraceHeader;
#[update]
fn lock(header: TraceHeader, x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::WrapMethod), "got {:?}", rules);
}

#[test]
fn rule_1_skips_when_already_wrapped() {
    let src = r#"
#[trace_method]
#[update]
fn lock(header: TraceHeader, x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::WrapMethod), "got {:?}", rules);
}

#[test]
fn rule_1_does_not_fire_without_trace_header() {
    let src = r#"
#[update]
fn lock(x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::WrapMethod), "got {:?}", rules);
}

// ---------- Rule 1b: WrapMethodInsertHeader ----------

#[test]
fn rule_1b_fires_when_cdk_attr_but_no_trace_header() {
    let src = r#"
#[update]
fn lock(x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::WrapMethodInsertHeader), "got {:?}", rules);
    assert!(!rules.contains(&Rule::WrapMethod));
}

#[test]
fn rule_1b_does_not_fire_without_cdk_attr() {
    let src = r#"
fn lock(x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::WrapMethodInsertHeader));
}

#[test]
fn rule_1b_does_not_fire_when_already_traced() {
    let src = r#"
#[update]
#[trace_method]
fn lock(header: TraceHeader, x: u64) {}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::WrapMethodInsertHeader));
    assert!(!rules.contains(&Rule::WrapMethod));
}

#[test]
fn rule_1b_replacement_inserts_attribute_and_param() {
    let src = "#[update]\nfn lock(x: u64) {}\n";
    let file = syn::parse_file(src).unwrap();
    let aliases = super::parse::collect_aliases(&file);
    let cands = super::detect::all(&file, src, &aliases);
    let c = cands
        .iter()
        .find(|c| c.rule == Rule::WrapMethodInsertHeader)
        .expect("rule 1b candidate");
    let new_src = super::edit::apply(src, std::slice::from_ref(c)).unwrap();
    assert!(new_src.contains("#[trace_method]"), "{}", new_src);
    assert!(
        new_src.contains("fn lock(header: TraceHeader, x: u64)"),
        "{}",
        new_src
    );
}

#[test]
fn rule_1b_replacement_handles_empty_param_list() {
    let src = "#[update]\nfn ping() {}\n";
    let file = syn::parse_file(src).unwrap();
    let aliases = super::parse::collect_aliases(&file);
    let cands = super::detect::all(&file, src, &aliases);
    let c = cands
        .iter()
        .find(|c| c.rule == Rule::WrapMethodInsertHeader)
        .expect("rule 1b candidate");
    let new_src = super::edit::apply(src, std::slice::from_ref(c)).unwrap();
    assert!(
        new_src.contains("fn ping(header: TraceHeader)"),
        "{}",
        new_src
    );
}

// ---------- Rule 2: ConvertCall ----------

#[test]
fn rule_2_fires_on_full_path_call() {
    let src = r#"
#[trace_method]
fn parent(header: TraceHeader) {
    let _ = ic_cdk::call(target, "method", (1u64,));
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::ConvertCall), "got {:?}", rules);
}

#[test]
fn rule_2_fires_on_aliased_call() {
    let src = r#"
use ic_cdk::call as c;
#[trace_method]
fn parent(header: TraceHeader) {
    let _ = c(target, "method", (1u64,));
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::ConvertCall), "got {:?}", rules);
}

#[test]
fn rule_2_does_not_fire_on_unrelated_call() {
    let src = r#"
#[trace_method]
fn parent(header: TraceHeader) {
    let _ = my_helper(target, "method", (1u64,));
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::ConvertCall), "got {:?}", rules);
}

#[test]
fn rule_2_does_not_fire_outside_traced_fn() {
    let src = r#"
fn untraced() {
    let _ = ic_cdk::call(target, "method", (1u64,));
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::ConvertCall), "got {:?}", rules);
}

// ---------- Rule 3: EntryNote ----------

#[test]
fn rule_3_fires_when_first_stmt_is_not_entry_event() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    let x = 1;
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::EntryNote), "got {:?}", rules);
}

#[test]
fn rule_3_skips_when_entry_event_already_present() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    trace_event!("lock:enter");
    let x = 1;
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::EntryNote), "got {:?}", rules);
}

// ---------- Rule 4: SnapshotLocal ----------

#[test]
fn rule_4_fires_on_struct_literal_local() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    let payment = Payment { id: 1, status: 0 };
    let _ = payment;
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::SnapshotLocal), "got {:?}", rules);
}

#[test]
fn rule_4_does_not_fire_on_method_call_init() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    let payment = Payment::new(1, 0);
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::SnapshotLocal), "got {:?}", rules);
}

#[test]
fn rule_4_skips_when_snapshot_already_follows() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    let payment = Payment { id: 1, status: 0 };
    trace_state!("payment", &payment);
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::SnapshotLocal), "got {:?}", rules);
}

// ---------- Rule 5: RollbackNote ----------

#[test]
fn rule_5_fires_after_state_then_return_err() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    let payment = Payment { id: 1 };
    trace_state!("payment", &payment);
    return Err("nope".to_string());
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

#[test]
fn rule_5_does_not_fire_without_prior_trace_state() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    return Err("nope".to_string());
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

#[test]
fn rule_5_does_not_fire_on_question_mark_propagation() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    let payment = Payment { id: 1 };
    trace_state!("payment", &payment);
    foo()?;
    Ok(1)
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

// ---------- Rule 6: TrapNote ----------

#[test]
fn rule_6_fires_on_ic_cdk_trap() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    ic_cdk::trap("oh no");
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::TrapNote), "got {:?}", rules);
}

#[test]
fn rule_6_does_not_fire_on_unrelated_trap() {
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) {
    my_module::trap("oh no");
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::TrapNote), "got {:?}", rules);
}

#[test]
fn rule_4_skips_when_snapshot_follows_with_intermediate_stmt() {
    let src = r#"
#[trace_method]
fn lock_funds(header: TraceHeader) {
    let lock = Lock { id: 1 };
    LOCKS.with(|l| l.borrow_mut().insert(1, lock.clone()));
    trace_state!("lock", &lock);
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::SnapshotLocal), "got {:?}", rules);
}

// ---------- Rule 7: MutationSnapshot ----------

#[test]
fn rule_7_fires_on_thread_local_with_borrow_mut() {
    let src = r#"
thread_local! {
    static LOCKS: RefCell<()> = RefCell::new(());
}
#[trace_method]
fn lock_funds(header: TraceHeader) {
    LOCKS.with(|l| l.borrow_mut().insert(1, ()));
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::MutationSnapshot), "got {:?}", rules);
}

#[test]
fn rule_7_does_not_fire_on_borrow_read() {
    let src = r#"
thread_local! {
    static LOCKS: RefCell<()> = RefCell::new(());
}
#[trace_method]
fn read(header: TraceHeader) {
    LOCKS.with(|l| l.borrow().get(&1).cloned());
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::MutationSnapshot), "got {:?}", rules);
}

#[test]
fn rule_7_does_not_fire_on_unknown_container() {
    let src = r#"
#[trace_method]
fn lock_funds(header: TraceHeader) {
    OTHER.with(|l| l.borrow_mut().insert(1, ()));
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::MutationSnapshot), "got {:?}", rules);
}

#[test]
fn rule_7_skips_when_trace_state_already_follows() {
    let src = r#"
thread_local! {
    static LOCKS: RefCell<()> = RefCell::new(());
}
#[trace_method]
fn lock_funds(header: TraceHeader) {
    LOCKS.with(|l| l.borrow_mut().insert(1, ()));
    trace_state!("locks", &LOCKS);
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::MutationSnapshot), "got {:?}", rules);
}

#[test]
fn rule_7_does_not_fire_outside_traced_fn() {
    let src = r#"
thread_local! {
    static LOCKS: RefCell<()> = RefCell::new(());
}
fn untraced() {
    LOCKS.with(|l| l.borrow_mut().insert(1, ()));
}
"#;
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::MutationSnapshot), "got {:?}", rules);
}

// ---------- Motoko rules ----------

fn mo_rules(src: &str) -> Vec<Rule> {
    super::motoko::detect(src, std::path::Path::new("test.mo"))
        .into_iter()
        .map(|c| c.rule)
        .collect()
}

// A minimal actor skeleton that already has the tracer field.  Tests for
// individual rules add their own methods inside the body.
fn mo_actor(method_body: &str) -> String {
    format!(
        r#"import Trace "../../../../motoko/src/Trace";
persistent actor self {{
  transient let tracer = Trace.Tracer(Principal.fromActor(self));
  var balance : Nat = 0;
  var items : [Nat] = [];
  {method_body}
  public query func __debug_drain() : async Blob {{ tracer.drain() }};
}}"#
    )
}

// --- Rule 6: mo-rollback-note ---

#[test]
fn mo_rule_6_fires_on_throw_in_traced_method() {
    let src = mo_actor(
        r#"public shared(msg) func withdraw(header : ?Trace.TraceHeader, amount : Nat) : async Nat {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("withdraw", msg.caller, []);
    if (amount > 100) {
      throw Error.reject("too much");
    };
    tracer.methodExited(null);
    amount
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(rules.contains(&Rule::MoRollbackNote), "got {:?}", rules);
}

#[test]
fn mo_rule_6_skips_when_not_traced() {
    let src = mo_actor(
        r#"public shared(msg) func withdraw(header : ?Trace.TraceHeader, amount : Nat) : async Nat {
    if (amount > 100) {
      throw Error.reject("too much");
    };
    amount
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoRollbackNote), "got {:?}", rules);
}

#[test]
fn mo_rule_6_skips_when_rollback_note_already_present() {
    let src = mo_actor(
        r#"public shared(msg) func withdraw(header : ?Trace.TraceHeader, amount : Nat) : async Nat {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("withdraw", msg.caller, []);
    if (amount > 100) {
      tracer.note("withdraw:rollback");
      throw Error.reject("too much");
    };
    tracer.methodExited(null);
    amount
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoRollbackNote), "got {:?}", rules);
}

#[test]
fn mo_rule_6_does_not_match_throw_inside_string() {
    // "throw" inside a string literal must not trigger the rule.
    let src = mo_actor(
        r#"public shared(msg) func safe(header : ?Trace.TraceHeader) : async Text {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("safe", msg.caller, []);
    let msg2 = "will throw if needed";
    tracer.methodExited(null);
    msg2
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoRollbackNote), "got {:?}", rules);
}

// --- Rule 7: mo-mutation-snapshot ---

#[test]
fn mo_rule_7_fires_on_actor_var_assignment_in_traced_method() {
    let src = mo_actor(
        r#"public shared(msg) func deposit(header : ?Trace.TraceHeader, amount : Nat) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("deposit", msg.caller, []);
    balance := balance + amount;
    tracer.methodExited(null);
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

#[test]
fn mo_rule_7_skips_when_not_traced() {
    let src = mo_actor(
        r#"public shared(msg) func deposit(header : ?Trace.TraceHeader, amount : Nat) : async () {
    balance := balance + amount;
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

#[test]
fn mo_rule_7_skips_when_snapshot_already_follows() {
    let src = mo_actor(
        r#"public shared(msg) func deposit(header : ?Trace.TraceHeader, amount : Nat) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("deposit", msg.caller, []);
    balance := balance + amount;
    tracer.snapshotText("balance", debug_show balance);
    tracer.methodExited(null);
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

#[test]
fn mo_rule_7_skips_record_field_update() {
    // `record.field :=` must not fire — only bare var idents are safe.
    let src = mo_actor(
        r#"public shared(msg) func update_field(header : ?Trace.TraceHeader) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("update_field", msg.caller, []);
    let r = { field = 0 };
    r.field := 1;
    tracer.methodExited(null);
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

#[test]
fn mo_rule_7_skips_unknown_var() {
    // A `:=` on a var not declared at actor scope must not fire.
    let src = mo_actor(
        r#"public shared(msg) func go(header : ?Trace.TraceHeader) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("go", msg.caller, []);
    let localVar = 0;
    localVar := 1;
    tracer.methodExited(null);
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

#[test]
fn mo_rule_7_skips_local_shadow_of_actor_var() {
    // If the function declares `var balance` locally, `:=` targets the
    // local, not the actor-level state — must not fire.
    let src = mo_actor(
        r#"public shared(msg) func go(header : ?Trace.TraceHeader) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("go", msg.caller, []);
    var balance = 0;
    balance := 42;
    tracer.methodExited(null);
  };"#,
    );
    let rules = mo_rules(&src);
    assert!(!rules.contains(&Rule::MoMutationSnapshot), "got {:?}", rules);
}

// ---------- Rule 1b: init / post_upgrade coverage ----------

#[test]
fn rule_1b_fires_on_init_without_trace_header() {
    let src = r#"
#[init]
fn canister_init() {}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::WrapMethodInsertHeader), "got {:?}", rules);
}

#[test]
fn rule_1b_fires_on_post_upgrade_without_trace_header() {
    let src = r#"
#[post_upgrade]
fn canister_post_upgrade() {}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::WrapMethodInsertHeader), "got {:?}", rules);
}

#[test]
fn rule_1_fires_on_init_with_trace_header() {
    let src = r#"
#[init]
fn canister_init(header: TraceHeader) {}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::WrapMethod), "got {:?}", rules);
}

// ---------- Rule 5: nested-block state propagation ----------

#[test]
fn rule_5_fires_on_return_err_inside_if_block() {
    // The most common real-world shape: trace_state! at function level,
    // then return Err inside an if body.
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    let payment = Payment { id: 1 };
    trace_state!("payment", &payment);
    if !ok {
        return Err("bad".to_string());
    }
    Ok(1)
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

#[test]
fn rule_5_fires_on_return_err_inside_match_arm_block() {
    // Match arm with a braced body — the return Err is a Stmt inside a
    // Block, so the propagated state_traced flag reaches it.
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    let payment = Payment { id: 1 };
    trace_state!("payment", &payment);
    match something {
        Some(x) => Ok(x),
        None => {
            return Err("missing".to_string());
        }
    }
}
"#;
    let rules = detect_rules(src);
    assert!(rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

#[test]
fn rule_5_does_not_fire_when_state_is_inside_nested_block_only() {
    // trace_state! is INSIDE the if block, so a return Err OUTSIDE it
    // at function level (before trace_state!) must not fire.
    let src = r#"
#[trace_method]
fn lock(header: TraceHeader) -> Result<u64, String> {
    if false {
        let p = Payment { id: 1 };
        trace_state!("p", &p);
    }
    return Err("nope".to_string());
}
"#;
    // state_traced propagates *down* into nested blocks, not *up* back to
    // the parent. So the return Err at function level, which comes before
    // the if block containing trace_state!, must not see it as "traced".
    // (Whether it fires depends on source order; in this fixture the
    // return Err is at function level AFTER the if block in source order —
    // scan_block processes stmts in order, so after the if we recurse
    // into it and set state_traced=true in the child, but the parent's
    // local flag only sees trace_state! stmts at its own level. So this
    // return Err should not fire.)
    let rules = detect_rules(src);
    assert!(!rules.contains(&Rule::RollbackNote), "got {:?}", rules);
}

// ---------- Idempotency ----------

#[test]
fn fully_instrumented_method_yields_no_candidates() {
    let src = r#"
#[trace_method]
#[update]
fn lock(header: TraceHeader) {
    trace_event!("lock:enter");
    let payment = Payment { id: 1 };
    trace_state!("payment", &payment);
}
"#;
    let rules = detect_rules(src);
    assert!(
        rules.is_empty(),
        "expected no candidates on fully-instrumented method, got {:?}",
        rules
    );
}
