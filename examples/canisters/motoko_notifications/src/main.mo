/// Motoko mirror of the Rust `notifications` canister. Intentionally flaky
/// to surface the same async-callback bug the Rust reference demo uses.

import Principal "mo:base/Principal";
import Nat64 "mo:base/Nat64";
import Trie "mo:base/Trie";
import Text "mo:base/Text";
import Result "mo:base/Result";

import Trace "../../../../motoko/src/Trace";

persistent actor self {

  type Receipt = {
    payment_id : Nat64;
    delivered  : Bool;
  };

  transient let tracer = Trace.Tracer(Principal.fromActor(self));

  var receipts : Trie.Trie<Nat64, Receipt> = Trie.empty();
  var failNext : Bool = false;

  func key(n : Nat64) : Trie.Key<Nat64> {
    { hash = Text.hash(Nat64.toText(n)); key = n }
  };

  public shared (msg) func send_receipt(
    header     : ?Trace.TraceHeader,
    payment_id : Nat64,
  ) : async Result.Result<Receipt, Text> {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("send_receipt", msg.caller, [
      ("payment_id", debug_show payment_id),
    ]);
    tracer.note("notifications.send_receipt:enter");

    let shouldFail = failNext;
    failNext := false;

    if (shouldFail) {
      tracer.note("notifications.send_receipt:rejecting");
      tracer.methodExited(?"notification channel unavailable");
      return #err("notification channel unavailable");
    };

    let r : Receipt = { payment_id; delivered = true };
    receipts := Trie.put<Nat64, Receipt>(receipts, key(payment_id), Nat64.equal, r).0;
    tracer.snapshotText("receipt", debug_show r);

    tracer.methodExited(null);
    #ok(r)
  };

  /// Toggle: next `send_receipt` will reject. For reproducing the demo bug.
  public func arm_failure() : async () {
    failNext := true;
  };

  public query func get_receipt(payment_id : Nat64) : async ?Receipt {
    Trie.get<Nat64, Receipt>(receipts, key(payment_id), Nat64.equal)
  };

  public query func __debug_drain() : async Blob {
    tracer.drain()
  };

};
