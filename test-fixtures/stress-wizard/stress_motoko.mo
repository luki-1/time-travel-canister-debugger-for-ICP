// Stress fixture for the Motoko side of `ic-debug instrument`.
//
// The actor already has the bootstrap (`Trace` import, `tracer` field,
// `__debug_drain` query) so M1 does NOT fire here — see
// `stress_motoko_bootstrap.mo` for the M1 case. This file exercises
// M2..M7 plus their negatives. Expected candidates live in
// `expected_motoko.md`.

import Debug "mo:base/Debug";
import Array "mo:base/Array";
import Error "mo:base/Error";
import Trace "../../motoko/src/Trace";

actor StressMotoko {

    transient let tracer = Trace.Tracer(64);

    public query func __debug_drain() : async Blob { tracer.drain() };

    // Actor-level vars (depth 0 of the actor body). M7 may fire on
    // assignments to these from inside traced method bodies.
    var balance : Nat = 0;
    var owner   : Text = "";
    var items   : [Nat] = [];

    // ====================================================================
    // M2 — public method with ?Trace.TraceHeader, no tracer.beginTrace yet.
    // Should fire: insert begin/methodEntered/methodExited boilerplate.
    // ====================================================================
    public func m2_wrap_method(header : ?Trace.TraceHeader, x : Nat) : async Nat {
        let _ = header;
        x + 1
    };

    // ====================================================================
    // M3 — public method WITHOUT ?Trace.TraceHeader.
    // Should fire (with breaking-ABI warning): insert header param +
    // boilerplate.
    // ====================================================================
    public func m3_insert_header(x : Nat) : async Nat {
        x + 2
    };

    // M3 negative — query func is excluded by design.
    public query func neg_query_excluded(x : Nat) : async Nat {
        x + 3
    };

    // M3 negative — __debug_drain is excluded by design (above).

    // ====================================================================
    // M4 — already wrapped (has tracer.beginTrace), but body has no
    // tracer.note("…:enter") near the top. Should fire.
    // ====================================================================
    public func m4_entry_note(header : ?Trace.TraceHeader) : async Nat {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m4_entry_note"; caller = ""; argsHash = "" });
        let n = 7;
        tracer.methodExited(null);
        n
    };

    // ====================================================================
    // M5 — Debug.trap inside a traced body. Should fire: insert
    // tracer.note("m5_trap:trapped") on the line above.
    // ====================================================================
    public func m5_trap(header : ?Trace.TraceHeader, n : Nat) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m5_trap"; caller = ""; argsHash = "" });
        tracer.note("m5_trap:enter");
        if (n == 0) {
            Debug.trap("zero not allowed");
        };
        tracer.methodExited(null);
    };

    // M5 negative — Debug.trap inside a non-traced function. Must NOT fire.
    public func m5_negative_not_traced(n : Nat) : async () {
        if (n == 0) {
            Debug.trap("not traced, do not flag");
        };
    };

    // ====================================================================
    // M6 — `throw` inside a traced body. Should fire: insert
    // tracer.note("m6_rollback:rollback") above the throw.
    // ====================================================================
    public func m6_rollback(header : ?Trace.TraceHeader, ok : Bool) : async Nat {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m6_rollback"; caller = ""; argsHash = "" });
        tracer.note("m6_rollback:enter");
        if (not ok) {
            throw Error.reject("bad");
        };
        tracer.methodExited(null);
        0
    };

    // M6 negative — `throw` inside a string literal. Must NOT fire.
    public func m6_negative_string(header : ?Trace.TraceHeader) : async Text {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m6_negative_string"; caller = ""; argsHash = "" });
        tracer.note("m6_negative_string:enter");
        let msg = "we throw an error here";
        tracer.methodExited(null);
        msg
    };

    // M6 negative — already has a :rollback note above the throw. Must NOT fire.
    public func m6_negative_already_noted(header : ?Trace.TraceHeader) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m6_negative_already_noted"; caller = ""; argsHash = "" });
        tracer.note("m6_negative_already_noted:enter");
        tracer.note("m6_negative_already_noted:rollback");
        throw Error.reject("bad");
    };

    // M6 negative — `throw` inside a non-traced function. Must NOT fire.
    public func m6_negative_not_traced(ok : Bool) : async () {
        if (not ok) {
            throw Error.reject("not traced");
        };
    };

    // ====================================================================
    // M7 — actor-var assignment inside a traced body. Should fire on the
    // bare-ident `balance := …` and `owner := …` lines.
    // ====================================================================
    public func m7_mutation(header : ?Trace.TraceHeader, n : Nat, who : Text) : async Nat {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_mutation"; caller = ""; argsHash = "" });
        tracer.note("m7_mutation:enter");
        balance := balance + n;
        owner := who;
        tracer.methodExited(null);
        balance
    };

    // M7 negative — record-field update. Must NOT fire (record.field := …).
    public func m7_negative_record(header : ?Trace.TraceHeader) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_negative_record"; caller = ""; argsHash = "" });
        tracer.note("m7_negative_record:enter");
        let record = { var field = 0 };
        record.field := 5;
        tracer.methodExited(null);
    };

    // M7 negative — array-element update. Must NOT fire (arr[i] := …).
    public func m7_negative_array(header : ?Trace.TraceHeader) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_negative_array"; caller = ""; argsHash = "" });
        tracer.note("m7_negative_array:enter");
        let arr = Array.init<Nat>(3, 0);
        arr[0] := 5;
        tracer.methodExited(null);
    };

    // M7 negative — local var shadow of actor-level `balance`. Must NOT fire.
    public func m7_negative_shadow(header : ?Trace.TraceHeader) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_negative_shadow"; caller = ""; argsHash = "" });
        tracer.note("m7_negative_shadow:enter");
        var balance : Nat = 99;
        balance := balance + 1;
        tracer.methodExited(null);
    };

    // M7 negative — assignment to an unknown ident (not an actor var). Must NOT fire.
    public func m7_negative_unknown(header : ?Trace.TraceHeader) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_negative_unknown"; caller = ""; argsHash = "" });
        tracer.note("m7_negative_unknown:enter");
        var local_only : Nat = 0;
        local_only := 42;
        tracer.methodExited(null);
    };

    // M7 negative — assignment already followed by a snapshotText. Must NOT fire.
    public func m7_negative_already_snapshotted(header : ?Trace.TraceHeader, n : Nat) : async () {
        switch (header) {
            case (?h) tracer.beginTrace(h);
            case null {};
        };
        tracer.methodEntered({ method = "m7_negative_already_snapshotted"; caller = ""; argsHash = "" });
        tracer.note("m7_negative_already_snapshotted:enter");
        balance := balance + n;
        tracer.snapshotText("balance", debug_show balance);
        tracer.methodExited(null);
    };

};
