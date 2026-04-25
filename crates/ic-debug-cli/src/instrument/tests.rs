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
