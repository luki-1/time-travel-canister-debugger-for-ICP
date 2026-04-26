/// Motoko mirror of the Rust `ledger` canister. Named accounts with
/// Nat64 balances. `transfer` silently returns false on insufficient funds —
/// no `note` is emitted in that branch, mirroring the Rust demo bug.

import Principal "mo:base/Principal";
import Nat64 "mo:base/Nat64";
import Trie "mo:base/Trie";
import Text "mo:base/Text";

import Trace "../../../../motoko/src/Trace";

persistent actor self {

  transient let tracer = Trace.Tracer(Principal.fromActor(self));

  var accounts : Trie.Trie<Text, Nat64> = Trie.empty();

  func tkey(s : Text) : Trie.Key<Text> {
    { hash = Text.hash(s); key = s }
  };

  /// Credit `amount` to `account`, creating it if needed. Always succeeds.
  public shared (msg) func deposit(
    header  : ?Trace.TraceHeader,
    account : Text,
    amount  : Nat64,
  ) : async () {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("deposit", msg.caller, [
      ("account", debug_show account),
      ("amount",  debug_show amount),
    ]);

    let cur : Nat64 = switch (Trie.get<Text, Nat64>(accounts, tkey(account), Text.equal)) {
      case null 0;
      case (?v) v;
    };
    let next = cur + amount;
    accounts := Trie.put<Text, Nat64>(accounts, tkey(account), Text.equal, next).0;
    tracer.snapshotText("account/" # account, debug_show next);

    tracer.methodExited(null);
  };

  /// Move `amount` from `from` to `to`. Returns `true` on success.
  ///
  /// Silent failure when `from` has insufficient funds: no note is emitted,
  /// mirroring the deliberate bug in the Rust reference demo.
  public shared (msg) func transfer(
    header : ?Trace.TraceHeader,
    from   : Text,
    to     : Text,
    amount : Nat64,
  ) : async Bool {
    ignore tracer.beginTrace(header);
    tracer.methodEntered("transfer", msg.caller, [
      ("from",   debug_show from),
      ("to",     debug_show to),
      ("amount", debug_show amount),
    ]);

    let fromBal : Nat64 = switch (Trie.get<Text, Nat64>(accounts, tkey(from), Text.equal)) {
      case null 0;
      case (?v) v;
    };
    if (fromBal < amount) {
      // Silent failure — no note here.
      tracer.methodExited(null);
      return false;
    };

    let newFrom = fromBal - amount;
    accounts := Trie.put<Text, Nat64>(accounts, tkey(from), Text.equal, newFrom).0;

    let toBal : Nat64 = switch (Trie.get<Text, Nat64>(accounts, tkey(to), Text.equal)) {
      case null 0;
      case (?v) v;
    };
    let newTo = toBal + amount;
    accounts := Trie.put<Text, Nat64>(accounts, tkey(to), Text.equal, newTo).0;

    tracer.snapshotText("account/" # from, debug_show newFrom);
    tracer.snapshotText("account/" # to,   debug_show newTo);

    tracer.methodExited(null);
    true
  };

  public query func balance(account : Text) : async ?Nat64 {
    Trie.get<Text, Nat64>(accounts, tkey(account), Text.equal)
  };

  public query func __debug_drain() : async Blob {
    tracer.drain()
  };

};
