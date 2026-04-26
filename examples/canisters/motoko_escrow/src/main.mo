/// Motoko mirror of the Rust `escrow` example, demonstrating ic-debug's
/// Motoko trace library. Locks funds for a payment, records every entry,
/// state mutation, and exit so the timeline UI can replay them.

import Principal "mo:base/Principal";
import Nat64 "mo:base/Nat64";
import Trie "mo:base/Trie";
import Text "mo:base/Text";

import Trace "../../../../motoko/src/Trace";

persistent actor self {

  type Lock = {
    payment_id : Nat64;
    amount     : Nat64;
    released   : Bool;
  };

  transient let tracer = Trace.Tracer(Principal.fromActor(self));

  var locks : Trie.Trie<Nat64, Lock> = Trie.empty();

  func key(n : Nat64) : Trie.Key<Nat64> {
    { hash = Text.hash(Nat64.toText(n)); key = n }
  };

  public shared (msg) func lock_funds(
    header     : ?Trace.TraceHeader,
    payment_id : Nat64,
    amount     : Nat64,
  ) : async Lock {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("lock_funds", msg.caller, [
      ("payment_id", debug_show payment_id),
      ("amount",     debug_show amount),
    ]);
    tracer.note("escrow.lock_funds:enter");

    let lock : Lock = { payment_id; amount; released = false };
    locks := Trie.put<Nat64, Lock>(locks, key(payment_id), Nat64.equal, lock).0;
    tracer.snapshotText("lock", debug_show lock);

    tracer.methodExited(null);
    lock
  };

  public shared (msg) func release(
    header     : ?Trace.TraceHeader,
    payment_id : Nat64,
  ) : async ?Lock {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("release", msg.caller, [
      ("payment_id", debug_show payment_id),
    ]);
    tracer.note("escrow.release:enter");

    let result = switch (Trie.get<Nat64, Lock>(locks, key(payment_id), Nat64.equal)) {
      case null null;
      case (?existing) {
        let updated : Lock = {
          payment_id = existing.payment_id;
          amount     = existing.amount;
          released   = true;
        };
        locks := Trie.put<Nat64, Lock>(locks, key(payment_id), Nat64.equal, updated).0;
        tracer.snapshotText("lock", debug_show updated);
        ?updated
      };
    };

    tracer.methodExited(null);
    result
  };

  public query func get_lock(payment_id : Nat64) : async ?Lock {
    Trie.get<Nat64, Lock>(locks, key(payment_id), Nat64.equal)
  };

  public query func __debug_drain() : async Blob {
    tracer.drain()
  };

};
