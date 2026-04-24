//! Ledger canister: named accounts with u64 balances.
//!
//! Used by example 04 to demonstrate a silent error: `transfer` fails when
//! the source account has insufficient funds and the failure branch emits
//! nothing the UI can flag. Method arguments are captured automatically by
//! `#[trace_method]` — no manual `trace_event!` is needed to echo them.

use ic_cdk_macros::{query, update};
use ic_debug_trace::core::TraceHeader;
use ic_debug_trace::{trace_method, trace_state};
use std::cell::RefCell;
use std::collections::BTreeMap;

thread_local! {
    static ACCOUNTS: RefCell<BTreeMap<String, u64>> = RefCell::new(BTreeMap::new());
}

/// Credit `amount` to `account`, creating it if it doesn't exist.
/// Always succeeds. Emits a state snapshot of the updated balance.
#[update]
#[trace_method]
fn deposit(header: TraceHeader, account: String, amount: u64) {
    let _ = header;
    ACCOUNTS.with(|a| {
        let mut m = a.borrow_mut();
        let bal = m.entry(account.clone()).or_insert(0);
        *bal += amount;
        trace_state!(format!("account/{account}"), bal);
    });
}

/// Move `amount` from `from` to `to`. Returns `true` on success.
///
/// If `from` does not exist or has insufficient funds the method returns
/// `false` and exits — no `trace_event!` is emitted in that branch, so
/// the UI produces no ⚠ marker. The failure is visible only as the
/// absence of STATE snapshots the caller would expect to see.
#[update]
#[trace_method]
fn transfer(header: TraceHeader, from: String, to: String, amount: u64) -> bool {
    let _ = header;
    ACCOUNTS.with(|a| {
        let mut m = a.borrow_mut();
        let from_bal = *m.get(&from).unwrap_or(&0);
        if from_bal < amount {
            // Silent failure: no trace_event! here.
            return false;
        }
        *m.get_mut(&from).unwrap() -= amount;
        let new_from = *m.get(&from).unwrap();
        let to_bal = m.entry(to.clone()).or_insert(0);
        *to_bal += amount;
        let new_to = *to_bal;
        trace_state!(format!("account/{from}"), &new_from);
        trace_state!(format!("account/{to}"), &new_to);
        true
    })
}

/// Current balance for `account`, or `None` if it has never been deposited to.
#[query]
fn balance(account: String) -> Option<u64> {
    ACCOUNTS.with(|a| a.borrow().get(&account).copied())
}

#[query]
fn __debug_drain() -> Vec<u8> {
    ic_debug_trace::drain()
}

ic_cdk::export_candid!();
