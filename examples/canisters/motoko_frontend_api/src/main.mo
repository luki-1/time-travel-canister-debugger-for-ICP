/// Motoko mirror of the Rust `frontend_api`. Submits a payment that locks
/// funds in escrow and notifies the user. This canister is the ingress
/// surface for the Motoko pipeline; the Rust pipeline lives alongside it.
///
/// The intentional rollback bug from the Rust demo is preserved verbatim:
/// when `notifications.send_receipt` rejects, the payment is left in
/// `Locked` state and funds remain in escrow.

import Principal "mo:base/Principal";
import Nat64 "mo:base/Nat64";
import Trie "mo:base/Trie";
import Text "mo:base/Text";
import Result "mo:base/Result";
import Error "mo:base/Error";
import Debug "mo:base/Debug";
import Blob "mo:base/Blob";

import Trace "../../../../motoko/src/Trace";

persistent actor self {

  type PaymentStatus = {
    #Pending;
    #Locked;
    #Completed;
    #Failed;
  };

  type Payment = {
    id     : Nat64;
    amount : Nat64;
    status : PaymentStatus;
  };

  type Lock = {
    payment_id : Nat64;
    amount     : Nat64;
    released   : Bool;
  };

  type Receipt = {
    payment_id : Nat64;
    delivered  : Bool;
  };

  type EscrowService = actor {
    lock_funds : (?Trace.TraceHeader, Nat64, Nat64) -> async Lock;
  };

  type NotificationsService = actor {
    send_receipt : (?Trace.TraceHeader, Nat64) -> async Result.Result<Receipt, Text>;
  };

  transient let tracer = Trace.Tracer(Principal.fromActor(self));

  var nextId   : Nat64 = 0;
  var payments : Trie.Trie<Nat64, Payment> = Trie.empty();
  var escrow        : ?Principal = null;
  var notifications : ?Principal = null;

  func key(n : Nat64) : Trie.Key<Nat64> {
    { hash = Text.hash(Nat64.toText(n)); key = n }
  };

  public func configure(escrowId : Principal, notificationsId : Principal) : async () {
    escrow        := ?escrowId;
    notifications := ?notificationsId;
  };

  public shared (msg) func submit_payment(
    header : ?Trace.TraceHeader,
    amount : Nat64,
  ) : async Payment {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("submit_payment", msg.caller, [
      ("amount", debug_show amount),
    ]);
    tracer.note("submit_payment:enter");

    nextId += 1;
    let id = nextId;
    let pending : Payment = { id; amount; status = #Pending };
    payments := Trie.put<Nat64, Payment>(payments, key(id), Nat64.equal, pending).0;
    tracer.snapshotText("payment", debug_show pending);

    let escrowId = switch (escrow) {
      case null { Debug.trap("escrow not configured") };
      case (?p) p;
    };
    let notifId = switch (notifications) {
      case null { Debug.trap("notifications not configured") };
      case (?p) p;
    };

    // 1) Lock funds.
    let escrowHeader = tracer.callSpawned(escrowId, "lock_funds", "" : Blob);
    let escrowActor : EscrowService = actor (Principal.toText(escrowId));
    let lock = await escrowActor.lock_funds(?escrowHeader, id, amount);
    tracer.callReturned(null);
    tracer.snapshotText("lock", debug_show lock);

    let locked : Payment = { id; amount; status = #Locked };
    payments := Trie.put<Nat64, Payment>(payments, key(id), Nat64.equal, locked).0;
    tracer.snapshotText("payment", debug_show locked);

    // 2) Send receipt.
    let notifHeader = tracer.callSpawned(notifId, "send_receipt", "" : Blob);
    let notifActor : NotificationsService = actor (Principal.toText(notifId));
    let receiptOutcome : Result.Result<Receipt, Text> = try {
      let r = await notifActor.send_receipt(?notifHeader, id);
      tracer.callReturned(null);
      r
    } catch e {
      let m = Error.message(e);
      tracer.callReturned(?m);
      #err(m)
    };

    let final_ : Payment = switch (receiptOutcome) {
      case (#ok r) {
        tracer.snapshotText("receipt", debug_show r);
        let completed : Payment = { id; amount; status = #Completed };
        payments := Trie.put<Nat64, Payment>(payments, key(id), Nat64.equal, completed).0;
        tracer.snapshotText("payment", debug_show completed);
        completed
      };
      case (#err _msg) {
        // BUG intentionally left in for the reference demo: no rollback,
        // status stays Locked, funds stay in escrow.
        tracer.note("submit_payment:rollback_missing");
        locked
      };
    };

    tracer.methodExited(null);
    final_
  };

  public query func get_payment(id : Nat64) : async ?Payment {
    Trie.get<Nat64, Payment>(payments, key(id), Nat64.equal)
  };

  public query func __debug_drain() : async Blob {
    tracer.drain()
  };

};
