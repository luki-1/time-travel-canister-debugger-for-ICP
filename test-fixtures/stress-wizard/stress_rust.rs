//! Stress fixture for the Rust side of `ic-debug instrument`.
//!
//! Each annotated section is a deliberate test case for one rule, with both
//! positive cases (the rule should fire) and negative cases (it must not).
//! The list of expected candidates lives in `expected_rust.txt` next to
//! this file. Re-running the wizard on this file after applying once must
//! produce zero candidates (idempotency).

use std::cell::RefCell;
use candid::Principal;

// Stand-ins so the file parses without the real ic-debug-trace crate.
#[allow(non_camel_case_types)]
struct TraceHeader;

macro_rules! trace_event { ($($t:tt)*) => {}; }
macro_rules! trace_state { ($($t:tt)*) => {}; }
macro_rules! call_traced { ($($t:tt)*) => { Ok::<((),), String>(((),)) }; }

// CDK attribute stand-ins — the wizard only looks at the attribute name.
macro_rules! cdk_stub {
    ($name:ident) => {
        #[allow(unused_attributes)]
        #[doc = stringify!($name)]
        struct $name;
    };
}

// thread_local containers used by Rule 7 cases.
thread_local! {
    static COUNTER: RefCell<u64> = RefCell::new(0);
    static USERS: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

mod ic_cdk {
    use super::Principal;
    pub async fn call<A, R>(_: Principal, _: &str, _: A) -> Result<R, String> {
        unimplemented!()
    }
    pub fn trap(_: &str) -> ! { unreachable!() }
    pub mod api { pub fn trap(_: &str) -> ! { unreachable!() } }
}

mod helpers {
    use super::Principal;
    pub async fn send<A, R>(_: Principal, _: &str, _: A) -> Result<R, String> {
        unimplemented!()
    }
}

// =====================================================================
// Rule 1 — wrap-method
// Should fire: function has TraceHeader as first param, no #[trace_method].
// =====================================================================
#[update]
fn rule_1_wrap(header: TraceHeader, x: u64) -> u64 {
    let _ = header;
    x + 1
}

// =====================================================================
// Rule 1b — wrap-method-insert-header
// Should fire (with warning): #[update] without TraceHeader.
// =====================================================================
#[update]
fn rule_1b_insert_header(amount: u64) -> u64 { amount * 2 }

// Negative: Rule 1b excludes #[query]. Should NOT fire.
#[query]
fn neg_query_excluded(_: u64) -> u64 { 0 }

// Rule 1b also fires on #[init] — should fire here.
#[init]
fn rule_1b_init() {}

// =====================================================================
// Idempotence: already fully instrumented. NO rule should fire.
// =====================================================================
#[trace_method]
#[update]
fn already_instrumented(header: TraceHeader, x: u64) -> u64 {
    let _ = header;
    trace_event!("already_instrumented:enter");
    x + 4
}

// =====================================================================
// Rule 2 — convert-call (ic_cdk::call inside #[trace_method])
// =====================================================================
#[trace_method]
#[update]
async fn rule_2_call(header: TraceHeader, target: Principal) -> Result<u64, String> {
    let _ = header;
    trace_event!("rule_2_call:enter");
    let (n,): (u64,) = ic_cdk::call(target, "remote_method", (42u64,))
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(n)
}

// Rule 2 negative: ic_cdk::call inside string / comment / via helper wrapper.
// None of these should fire.
#[trace_method]
#[update]
fn rule_2_negative(header: TraceHeader) -> u64 {
    let _ = header;
    trace_event!("rule_2_negative:enter");
    let _description = "ic_cdk::call(target, \"x\", ())"; // string literal
    // ic_cdk::call(target, "x", ()) -- comment
    let _ = helpers::send::<(u64,), u64>; // helper wrapper, not a literal call
    0
}

// =====================================================================
// Rule 3 — entry-note (body's first stmt is not trace_event!(...:enter))
// Should fire: #[trace_method] body has no entry note.
// =====================================================================
#[trace_method]
#[update]
fn rule_3_entry_note(header: TraceHeader) -> u64 {
    let _ = header;
    let x = 7u64;
    x
}

// =====================================================================
// Rule 4 — snapshot-local (let p = SomeStruct { ... })
// Should fire: bare struct-literal binding inside #[trace_method].
// =====================================================================
#[derive(Debug)]
struct Payment { id: u64, amount: u64 }

#[trace_method]
#[update]
fn rule_4_snapshot_local(header: TraceHeader) -> u64 {
    let _ = header;
    trace_event!("rule_4_snapshot_local:enter");
    let payment = Payment { id: 1, amount: 100 };
    payment.amount
}

// =====================================================================
// Rule 5 — rollback-note (return Err after a trace_state! in same block)
// Should fire on the `return Err`.
// =====================================================================
#[trace_method]
#[update]
fn rule_5_rollback(header: TraceHeader, ok: bool) -> Result<u64, String> {
    let _ = header;
    trace_event!("rule_5_rollback:enter");
    let p = Payment { id: 1, amount: 100 };
    trace_state!("payment", &p);
    if !ok {
        return Err("bad".into());
    }
    Ok(p.amount)
}

// Rule 5 negative: `?` operator should NOT trigger rollback-note.
#[trace_method]
#[update]
fn rule_5_negative_question_mark(header: TraceHeader, s: &str) -> Result<u64, String> {
    let _ = header;
    trace_event!("rule_5_negative_question_mark:enter");
    let p = Payment { id: 1, amount: 100 };
    trace_state!("payment", &p);
    let n = s.parse::<u64>().map_err(|e| e.to_string())?;
    Ok(n)
}

// Rule 5 negative: return Err WITHOUT a prior trace_state! — should NOT fire
// (Rule 5 only fires when state was snapshotted earlier in the block).
#[trace_method]
#[update]
fn rule_5_negative_no_state(header: TraceHeader, ok: bool) -> Result<u64, String> {
    let _ = header;
    trace_event!("rule_5_negative_no_state:enter");
    if !ok {
        return Err("no state to roll back".into());
    }
    Ok(0)
}

// =====================================================================
// Rule 6 — trap-note (ic_cdk::trap inside #[trace_method])
// =====================================================================
#[trace_method]
#[update]
fn rule_6_trap(header: TraceHeader, n: u64) {
    let _ = header;
    trace_event!("rule_6_trap:enter");
    if n == 0 {
        ic_cdk::trap("zero not allowed");
    }
}

// Rule 6 also fires on the alternate path: ic_cdk::api::trap.
#[trace_method]
#[update]
fn rule_6_trap_api(header: TraceHeader, n: u64) {
    let _ = header;
    trace_event!("rule_6_trap_api:enter");
    if n > 100 {
        ic_cdk::api::trap("too big");
    }
}

// =====================================================================
// Rule 7 — mutation-snapshot (thread_local NAME.with(...).borrow_mut())
// Should fire: COUNTER and USERS are both thread_local! in this file.
// =====================================================================
#[trace_method]
#[update]
fn rule_7_mutation(header: TraceHeader, n: u64, who: String) -> u64 {
    let _ = header;
    trace_event!("rule_7_mutation:enter");
    COUNTER.with(|c| *c.borrow_mut() += n);                // Rule 7 candidate #1
    USERS.with(|u| u.borrow_mut().push(who));              // Rule 7 candidate #2
    COUNTER.with(|c| *c.borrow())
}

// Rule 7 negative: a thread_local-shaped call against an UNKNOWN ident
// (not declared in this file) — should NOT fire.
#[trace_method]
#[update]
fn rule_7_negative_unknown(header: TraceHeader, n: u64) {
    let _ = header;
    trace_event!("rule_7_negative_unknown:enter");
    UNKNOWN_STATE.with(|s| *s.borrow_mut() += n);
}

// Stand-in so the file parses; UNKNOWN_STATE is intentionally NOT a
// thread_local declaration, so Rule 7 must reject it.
struct UnknownState;
impl UnknownState {
    fn with<R>(&self, _f: impl FnOnce(&RefCell<u64>) -> R) -> R {
        unimplemented!()
    }
}
#[allow(non_upper_case_globals)]
const UNKNOWN_STATE: UnknownState = UnknownState;

// =====================================================================
// Combined: a method with multiple rules firing at once.
// Should produce: Rule 1 (no #[trace_method]), Rule 2 (call), Rule 4
// (let payment), Rule 5 (return Err), Rule 6 (trap), Rule 7 (COUNTER.with).
// All inside one method that has TraceHeader but is not yet wrapped.
// =====================================================================
#[update]
async fn combined_method(
    header: TraceHeader,
    target: Principal,
    n: u64,
) -> Result<u64, String> {
    let _ = header;
    let payment = Payment { id: 1, amount: n };
    let (_r,): (u64,) = ic_cdk::call(target, "remote", (payment.amount,))
        .await
        .map_err(|e| format!("{:?}", e))?;
    if n == 0 {
        ic_cdk::trap("zero");
    }
    COUNTER.with(|c| *c.borrow_mut() += n);
    if n > 1000 {
        return Err("too big".into());
    }
    Ok(n)
}

// Stand-in CDK attribute macros so the file parses standalone.
cdk_stub!(stub_marker);
