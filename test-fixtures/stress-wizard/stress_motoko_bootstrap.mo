// Minimal Motoko fixture for the M1 (mo-bootstrap) rule.
// No `import Trace`, no tracer field, no __debug_drain — M1 should fire.

actor Bootstrap {
    var counter : Nat = 0;

    public func bump() : async Nat {
        counter := counter + 1;
        counter
    };
};
