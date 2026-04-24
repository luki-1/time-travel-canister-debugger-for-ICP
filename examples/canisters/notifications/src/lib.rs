//! Notifications canister: delivers receipts. Intentionally flaky to surface
//! the async-callback bug described in the MVP user flow.

use candid::CandidType;
use ic_cdk_macros::{query, update};
use ic_debug_trace::core::TraceHeader;
use ic_debug_trace::{trace_event, trace_method, trace_state};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Debug, CandidType, Serialize, Deserialize)]
pub struct Receipt {
    pub payment_id: u64,
    pub delivered: bool,
}

thread_local! {
    static RECEIPTS: RefCell<BTreeMap<u64, Receipt>> = RefCell::new(BTreeMap::new());
    static FAIL_NEXT: RefCell<bool> = const { RefCell::new(false) };
}

#[update]
#[trace_method]
fn send_receipt(header: TraceHeader, payment_id: u64) -> Result<Receipt, String> {
    let _ = header; // adopted by #[trace_method]
    trace_event!("notifications.send_receipt:enter");
    let should_fail = FAIL_NEXT.with(|f| {
        let prev = *f.borrow();
        *f.borrow_mut() = false;
        prev
    });
    if should_fail {
        trace_event!("notifications.send_receipt:rejecting");
        return Err("notification channel unavailable".to_string());
    }
    let r = Receipt { payment_id, delivered: true };
    RECEIPTS.with(|rs| rs.borrow_mut().insert(payment_id, r.clone()));
    trace_state!("receipt", &r);
    Ok(r)
}

/// Toggle: next `send_receipt` will reject. For reproducing the demo bug.
#[update]
fn arm_failure() {
    FAIL_NEXT.with(|f| *f.borrow_mut() = true);
}

#[query]
fn get_receipt(payment_id: u64) -> Option<Receipt> {
    RECEIPTS.with(|rs| rs.borrow().get(&payment_id).cloned())
}

#[query]
fn __debug_drain() -> Vec<u8> {
    ic_debug_trace::drain()
}

ic_cdk::export_candid!();
