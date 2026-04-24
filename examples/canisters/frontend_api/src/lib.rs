//! Reference sample: submits a payment that locks funds in escrow and then
//! notifies the user. This canister is the ingress surface.

use candid::{CandidType, Principal};
use ic_cdk_macros::{query, update};
use ic_debug_trace::core::TraceHeader;
use ic_debug_trace::{call_traced, trace_event, trace_state, trace_method};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, CandidType, Serialize, Deserialize, PartialEq, Eq)]
pub enum PaymentStatus {
    Pending,
    Locked,
    Completed,
    Failed,
}

#[derive(Clone, Debug, CandidType, Serialize, Deserialize)]
pub struct Payment {
    pub id: u64,
    pub amount: u64,
    pub status: PaymentStatus,
}

#[derive(Clone, Debug, CandidType, Serialize, Deserialize)]
pub struct Lock {
    pub payment_id: u64,
    pub amount: u64,
    pub released: bool,
}

#[derive(Clone, Debug, CandidType, Serialize, Deserialize)]
pub struct Receipt {
    pub payment_id: u64,
    pub delivered: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[derive(Default)]
struct State {
    next_id: u64,
    payments: BTreeMap<u64, Payment>,
    escrow: Option<Principal>,
    notifications: Option<Principal>,
}

#[update]
fn configure(escrow: Principal, notifications: Principal) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.escrow = Some(escrow);
        s.notifications = Some(notifications);
    });
}

#[update]
#[trace_method]
async fn submit_payment(header: TraceHeader, amount: u64) -> Payment {
    // `begin_trace(header)` is injected by #[trace_method]; silence the unused warning.
    let _ = header;
    trace_event!("submit_payment:enter");

    let (id, payment) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.next_id += 1;
        let p = Payment { id: s.next_id, amount, status: PaymentStatus::Pending };
        s.payments.insert(p.id, p.clone());
        (p.id, p)
    });
    trace_state!("payment", &payment);

    let (escrow, notifications) = STATE.with(|s| {
        let s = s.borrow();
        (s.escrow.expect("escrow not configured"), s.notifications.expect("notifications not configured"))
    });

    // 1) Lock funds.
    let lock: Result<(Lock,), _> = call_traced!(escrow, "lock_funds", (id, amount));
    let lock = lock.expect("escrow.lock_funds failed");
    trace_state!("lock", &lock.0);

    STATE.with(|s| {
        if let Some(p) = s.borrow_mut().payments.get_mut(&id) {
            p.status = PaymentStatus::Locked;
            trace_state!("payment", p);
        }
    });

    // 2) Send receipt.
    let receipt: Result<(Result<Receipt, String>,), _> =
        call_traced!(notifications, "send_receipt", (id,));
    match receipt {
        Ok((Ok(r),)) => {
            trace_state!("receipt", &r);
            STATE.with(|s| {
                if let Some(p) = s.borrow_mut().payments.get_mut(&id) {
                    p.status = PaymentStatus::Completed;
                    trace_state!("payment", p);
                }
            });
        }
        Ok((Err(msg),)) | Err((_, msg)) => {
            trace_event!("submit_payment:rollback_missing");
            // BUG intentionally left in for the reference demo: no rollback
            // is performed here. Status stays Locked, funds stay in escrow.
            let _ = msg;
        }
    }

    STATE.with(|s| s.borrow().payments.get(&id).cloned().unwrap())
}

#[query]
fn get_payment(id: u64) -> Option<Payment> {
    STATE.with(|s| s.borrow().payments.get(&id).cloned())
}

#[query]
fn __debug_drain() -> Vec<u8> {
    ic_debug_trace::drain()
}

ic_cdk::export_candid!();
