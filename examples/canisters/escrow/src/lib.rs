//! Escrow canister: locks funds for a payment.

use candid::CandidType;
use ic_cdk_macros::{query, update};
use ic_debug_trace::core::TraceHeader;
use ic_debug_trace::{trace_event, trace_method, trace_state};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Debug, CandidType, Serialize, Deserialize)]
pub struct Lock {
    pub payment_id: u64,
    pub amount: u64,
    pub released: bool,
}

thread_local! {
    static LOCKS: RefCell<BTreeMap<u64, Lock>> = RefCell::new(BTreeMap::new());
}

#[update]
#[trace_method]
fn lock_funds(header: TraceHeader, payment_id: u64, amount: u64) -> Lock {
    let _ = header; // adopted by #[trace_method] via injected begin_trace
    trace_event!("escrow.lock_funds:enter");
    let lock = Lock { payment_id, amount, released: false };
    LOCKS.with(|l| l.borrow_mut().insert(payment_id, lock.clone()));
    trace_state!("lock", &lock);
    lock
}

#[update]
#[trace_method]
fn release(header: TraceHeader, payment_id: u64) -> Option<Lock> {
    let _ = header; // adopted by #[trace_method]
    trace_event!("escrow.release:enter");
    LOCKS.with(|l| {
        let mut m = l.borrow_mut();
        if let Some(lock) = m.get_mut(&payment_id) {
            lock.released = true;
            trace_state!("lock", lock);
            Some(lock.clone())
        } else {
            None
        }
    })
}

#[query]
fn get_lock(payment_id: u64) -> Option<Lock> {
    LOCKS.with(|l| l.borrow().get(&payment_id).cloned())
}

#[query]
fn __debug_drain() -> Vec<u8> {
    ic_debug_trace::drain()
}

ic_cdk::export_candid!();
