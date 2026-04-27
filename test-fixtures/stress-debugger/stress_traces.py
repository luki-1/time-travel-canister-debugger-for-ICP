"""
Stress trace generator for the ic-debug debugger.

Five synthetic traces designed to exercise corners of the recorder, replay,
diff, and serve API layers. Each one targets a specific shape:

  S1  deep_chain   — A → B → C → D → E (5-level cross-canister chain)
                     verifies parent_seq propagates across all hops and the
                     replay splicer reconstructs a depth-5 call tree.

  S2  fan_out      — A spawns 12 sequential calls to B with different args
                     verifies many call_spawned events under the same span
                     all carry the correct parent_seq.

  S3  high_volume  — single canister, ~600 events, ~80 snapshots
                     stresses snapshot decoding, diff engine, and the
                     /api/traces/:id JSON payload size.

  S4  mixed_rejects — three reject paths in one trace:
                       (a) method_exited with reject (trap)
                       (b) call_returned with reject (IC-level reject)
                       (c) note with label …:rollback (Candid Result::Err)
                     verifies UI flagging picks all three.

  S5  many_keys    — single method snapshots 25 distinct state keys
                     verifies state panel renders many keys without
                     dropping any.

Run from the repo root:
  C:/Python312/python.exe test-fixtures/stress-debugger/stress_traces.py
"""

import json
import os
import sqlite3
import sys
import time

DB = os.path.join(os.path.dirname(__file__), "stress_traces.sqlite")

# Test canister principals (deterministic order matters for causal-root
# selection in replay).
CAN_A = "aaaaa-aaaaa-aaaaa-aaaaa-cai"
CAN_B = "bbbbb-bbbbb-bbbbb-bbbbb-cai"
CAN_C = "ccccc-ccccc-ccccc-ccccc-cai"
CAN_D = "ddddd-ddddd-ddddd-ddddd-cai"
CAN_E = "eeeee-eeeee-eeeee-eeeee-cai"
AGENT = "2vxsx-fae"

NAMES = {
    CAN_A: "stress.a",
    CAN_B: "stress.b",
    CAN_C: "stress.c",
    CAN_D: "stress.d",
    CAN_E: "stress.e",
}

TRACE_IDS = {
    "deep_chain":     "30000001-strs-0000-0000-000000000001",
    "fan_out":        "30000002-strs-0000-0000-000000000002",
    "high_volume":    "30000003-strs-0000-0000-000000000003",
    "mixed_rejects":  "30000004-strs-0000-0000-000000000004",
    "many_keys":      "30000005-strs-0000-0000-000000000005",
}
TRACE_LABELS = {
    "deep_chain":    "stress: 5-level deep call chain (A→B→C→D→E)",
    "fan_out":       "stress: 12-way fan-out from one parent span",
    "high_volume":   "stress: 600 events / 80 snapshots in one trace",
    "mixed_rejects": "stress: trap + IC reject + Candid Err in one trace",
    "many_keys":     "stress: 25 distinct state keys in one method",
}

SCHEMA = """
CREATE TABLE IF NOT EXISTS traces (
    id TEXT PRIMARY KEY,
    started_at INTEGER NOT NULL,
    label TEXT
);
CREATE TABLE IF NOT EXISTS events (
    trace_id TEXT NOT NULL,
    canister TEXT NOT NULL,
    seq INTEGER NOT NULL,
    parent_seq INTEGER,
    span_id INTEGER NOT NULL,
    ts_nanos TEXT NOT NULL,
    recv_nanos TEXT,
    kind TEXT NOT NULL,
    method TEXT,
    caller TEXT,
    target TEXT,
    reject TEXT,
    snapshot_key TEXT,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (trace_id, canister, seq)
);
CREATE INDEX IF NOT EXISTS idx_events_trace ON events(trace_id, ts_nanos);
CREATE TABLE IF NOT EXISTS snapshots (
    trace_id TEXT NOT NULL,
    canister TEXT NOT NULL,
    seq INTEGER NOT NULL,
    key TEXT NOT NULL,
    cbor BLOB NOT NULL,
    PRIMARY KEY (trace_id, canister, seq, key)
);
CREATE TABLE IF NOT EXISTS canister_names (
    principal TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
"""


# ── CBOR encoding (subset) ───────────────────────────────────────────────────

def cbor_encode(val) -> bytes:
    if val is None:                      return bytes([0xf6])
    if isinstance(val, bool):            return bytes([0xf5 if val else 0xf4])
    if isinstance(val, int):
        if val >= 0:
            if val < 24:        return bytes([val])
            if val < 0x100:     return bytes([0x18, val])
            if val < 0x10000:   return bytes([0x19]) + val.to_bytes(2, "big")
            if val < 2**32:     return bytes([0x1a]) + val.to_bytes(4, "big")
            return bytes([0x1b]) + val.to_bytes(8, "big")
        n = -val - 1
        if n < 24:        return bytes([0x20 | n])
        if n < 0x100:     return bytes([0x38, n])
        if n < 0x10000:   return bytes([0x39]) + n.to_bytes(2, "big")
        if n < 2**32:     return bytes([0x3a]) + n.to_bytes(4, "big")
        return bytes([0x3b]) + n.to_bytes(8, "big")
    if isinstance(val, str):
        b = val.encode("utf-8")
        n = len(b)
        if n < 24:        hdr = bytes([0x60 | n])
        elif n < 0x100:   hdr = bytes([0x78, n])
        else:             hdr = bytes([0x79]) + n.to_bytes(2, "big")
        return hdr + b
    if isinstance(val, dict):
        n = len(val)
        if n < 24:        hdr = bytes([0xa0 | n])
        elif n < 0x100:   hdr = bytes([0xb8, n])
        else:             hdr = bytes([0xb9]) + n.to_bytes(2, "big")
        out = bytearray(hdr)
        for k, v in val.items():
            out += cbor_encode(k); out += cbor_encode(v)
        return bytes(out)
    if isinstance(val, list):
        n = len(val)
        if n < 24:        hdr = bytes([0x80 | n])
        elif n < 0x100:   hdr = bytes([0x98, n])
        else:             hdr = bytes([0x99]) + n.to_bytes(2, "big")
        out = bytearray(hdr)
        for el in val:
            out += cbor_encode(el)
        return bytes(out)
    raise TypeError(f"cbor_encode: unsupported {type(val)}")


# ── DB helpers ───────────────────────────────────────────────────────────────

def ev(**kw) -> str:
    return json.dumps(kw)


def ins(c, trace_id, can, seq, parent_seq, span_id, ts, kind,
        method=None, caller=None, target=None, reject=None,
        snap_key=None, payload=None):
    recv = ts + 3_000
    c.execute("""
        INSERT OR REPLACE INTO events
        (trace_id, canister, seq, parent_seq, span_id, ts_nanos, recv_nanos,
         kind, method, caller, target, reject, snapshot_key, payload_json)
        VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (trace_id, can, seq, parent_seq, span_id, str(ts), str(recv),
          kind, method, caller, target, reject, snap_key, payload))


def snap_ins(c, trace_id, can, seq, key, cbor_bytes):
    c.execute("INSERT OR REPLACE INTO snapshots VALUES (?,?,?,?,?)",
              (trace_id, can, seq, key, cbor_bytes))


def trace_row(c, trace_id, t0, label):
    c.execute("INSERT OR IGNORE INTO traces VALUES (?,?,?)",
              (trace_id, t0 // 1_000_000, label))


# ── Trace 1: deep_chain ──────────────────────────────────────────────────────

def build_deep_chain(c, t0):
    TID = TRACE_IDS["deep_chain"]
    trace_row(c, TID, t0, TRACE_LABELS["deep_chain"])
    tick = 1_000_000  # 1 ms

    # A enters at seq 0 (root span — parent_seq=None)
    ins(c, TID, CAN_A, 0, None, 1, t0,
        "method_entered", method="step", caller=AGENT,
        payload=ev(kind="method_entered", method="step", caller=AGENT, args=[["depth", "5"]]))
    ins(c, TID, CAN_A, 1, 0, 1, t0+tick,
        "note", payload=ev(kind="note", label="step:enter"))

    # A → B (call spawned at A seq 2)
    ins(c, TID, CAN_A, 2, 1, 1, t0+2*tick,
        "call_spawned", target=CAN_B, method="step",
        payload=ev(kind="call_spawned", target=CAN_B, method="step",
                   args_hash="aa01"))

    # B enters; parent_seq points at A's call_spawned (seq=2)
    ins(c, TID, CAN_B, 0, 2, 2, t0+3*tick,
        "method_entered", method="step", caller=CAN_A,
        payload=ev(kind="method_entered", method="step", caller=CAN_A,
                   args=[["depth", "4"]]))
    ins(c, TID, CAN_B, 1, 0, 2, t0+4*tick,
        "note", payload=ev(kind="note", label="step:enter"))

    # B → C
    ins(c, TID, CAN_B, 2, 1, 2, t0+5*tick,
        "call_spawned", target=CAN_C, method="step",
        payload=ev(kind="call_spawned", target=CAN_C, method="step",
                   args_hash="aa02"))

    ins(c, TID, CAN_C, 0, 2, 3, t0+6*tick,
        "method_entered", method="step", caller=CAN_B,
        payload=ev(kind="method_entered", method="step", caller=CAN_B,
                   args=[["depth", "3"]]))
    ins(c, TID, CAN_C, 1, 0, 3, t0+7*tick,
        "note", payload=ev(kind="note", label="step:enter"))

    # C → D
    ins(c, TID, CAN_C, 2, 1, 3, t0+8*tick,
        "call_spawned", target=CAN_D, method="step",
        payload=ev(kind="call_spawned", target=CAN_D, method="step",
                   args_hash="aa03"))

    ins(c, TID, CAN_D, 0, 2, 4, t0+9*tick,
        "method_entered", method="step", caller=CAN_C,
        payload=ev(kind="method_entered", method="step", caller=CAN_C,
                   args=[["depth", "2"]]))
    ins(c, TID, CAN_D, 1, 0, 4, t0+10*tick,
        "note", payload=ev(kind="note", label="step:enter"))

    # D → E (deepest hop)
    ins(c, TID, CAN_D, 2, 1, 4, t0+11*tick,
        "call_spawned", target=CAN_E, method="step",
        payload=ev(kind="call_spawned", target=CAN_E, method="step",
                   args_hash="aa04"))

    ins(c, TID, CAN_E, 0, 2, 5, t0+12*tick,
        "method_entered", method="step", caller=CAN_D,
        payload=ev(kind="method_entered", method="step", caller=CAN_D,
                   args=[["depth", "1"]]))
    ins(c, TID, CAN_E, 1, 0, 5, t0+13*tick,
        "note", payload=ev(kind="note", label="step:enter"))

    # E records a state snapshot, then exits.
    e_state = cbor_encode({"depth": 1, "complete": True, "result": 42})
    ins(c, TID, CAN_E, 2, 1, 5, t0+14*tick,
        "state_snapshot", snap_key="leaf",
        payload=ev(kind="state_snapshot", key="leaf", cbor=list(e_state)))
    snap_ins(c, TID, CAN_E, 2, "leaf", e_state)
    ins(c, TID, CAN_E, 3, 2, 5, t0+15*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))

    # Unwind the call stack: D returns, C returns, B returns, A returns.
    # Each call_returned points at the same canister's earlier call_spawned
    # via parent_seq (seq=2 for D, C, B, A's call rows).
    ins(c, TID, CAN_D, 3, 2, 4, t0+16*tick,
        "call_returned", payload=ev(kind="call_returned", reject=None))
    ins(c, TID, CAN_D, 4, 3, 4, t0+17*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))

    ins(c, TID, CAN_C, 3, 2, 3, t0+18*tick,
        "call_returned", payload=ev(kind="call_returned", reject=None))
    ins(c, TID, CAN_C, 4, 3, 3, t0+19*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))

    ins(c, TID, CAN_B, 3, 2, 2, t0+20*tick,
        "call_returned", payload=ev(kind="call_returned", reject=None))
    ins(c, TID, CAN_B, 4, 3, 2, t0+21*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))

    ins(c, TID, CAN_A, 3, 2, 1, t0+22*tick,
        "call_returned", payload=ev(kind="call_returned", reject=None))
    ins(c, TID, CAN_A, 4, 3, 1, t0+23*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))


# ── Trace 2: fan_out ─────────────────────────────────────────────────────────

def build_fan_out(c, t0):
    TID = TRACE_IDS["fan_out"]
    trace_row(c, TID, t0, TRACE_LABELS["fan_out"])
    tick = 1_000_000
    N = 12

    ins(c, TID, CAN_A, 0, None, 1, t0,
        "method_entered", method="dispatch", caller=AGENT,
        payload=ev(kind="method_entered", method="dispatch", caller=AGENT,
                   args=[["fan_out", str(N)]]))
    ins(c, TID, CAN_A, 1, 0, 1, t0+tick,
        "note", payload=ev(kind="note", label="dispatch:enter"))

    a_seq = 2
    b_seq = 0
    for i in range(N):
        ts = t0 + (2 + i*4) * tick
        # A spawns the i-th call to B
        ins(c, TID, CAN_A, a_seq, 1, 1, ts,
            "call_spawned", target=CAN_B, method="work",
            payload=ev(kind="call_spawned", target=CAN_B, method="work",
                       args_hash=f"f{i:03d}"))
        spawn_seq = a_seq
        a_seq += 1

        # B handles each call as its own span (span_id increments)
        ins(c, TID, CAN_B, b_seq, spawn_seq, 10+i, ts+tick,
            "method_entered", method="work", caller=CAN_A,
            payload=ev(kind="method_entered", method="work", caller=CAN_A,
                       args=[["task", str(i)]]))
        b_seq += 1
        ins(c, TID, CAN_B, b_seq, b_seq-1, 10+i, ts+2*tick,
            "note", payload=ev(kind="note", label=f"work:enter:task={i}"))
        b_seq += 1
        ins(c, TID, CAN_B, b_seq, b_seq-1, 10+i, ts+3*tick,
            "method_exited", payload=ev(kind="method_exited", reject=None))
        b_seq += 1

        # A's call_returned for this hop
        ins(c, TID, CAN_A, a_seq, spawn_seq, 1, ts+3*tick + 100,
            "call_returned", payload=ev(kind="call_returned", reject=None))
        a_seq += 1

    ins(c, TID, CAN_A, a_seq, a_seq-1, 1, t0 + (2 + N*4 + 1) * tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))


# ── Trace 3: high_volume ─────────────────────────────────────────────────────

def build_high_volume(c, t0):
    TID = TRACE_IDS["high_volume"]
    trace_row(c, TID, t0, TRACE_LABELS["high_volume"])
    tick = 1_000_000

    # Single canister, single method, N iterations of (note + snapshot).
    N = 200  # → 1 enter + 1 enter-note + 2*N + 1 exit ≈ 403 events; ~200 snaps
    ins(c, TID, CAN_A, 0, None, 1, t0,
        "method_entered", method="loop", caller=AGENT,
        payload=ev(kind="method_entered", method="loop", caller=AGENT,
                   args=[["iters", str(N)]]))
    ins(c, TID, CAN_A, 1, 0, 1, t0+tick,
        "note", payload=ev(kind="note", label="loop:enter"))

    seq = 2
    for i in range(N):
        ts = t0 + (2 + i*2) * tick
        # State snapshot of the loop counter, key = "counter"
        snap_cbor = cbor_encode({"counter": i, "doubled": i*2,
                                  "label": f"iteration #{i}",
                                  "history": list(range(min(i, 5)))})
        ins(c, TID, CAN_A, seq, seq-1, 1, ts,
            "state_snapshot", snap_key="counter",
            payload=ev(kind="state_snapshot", key="counter", cbor=list(snap_cbor)))
        snap_ins(c, TID, CAN_A, seq, "counter", snap_cbor)
        seq += 1

        ins(c, TID, CAN_A, seq, seq-1, 1, ts+tick,
            "note", payload=ev(kind="note", label=f"tick:i={i}"))
        seq += 1

    ins(c, TID, CAN_A, seq, seq-1, 1, t0 + (2 + N*2 + 1) * tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))


# ── Trace 4: mixed_rejects ───────────────────────────────────────────────────

def build_mixed_rejects(c, t0):
    TID = TRACE_IDS["mixed_rejects"]
    trace_row(c, TID, t0, TRACE_LABELS["mixed_rejects"])
    tick = 1_000_000

    # A enters
    ins(c, TID, CAN_A, 0, None, 1, t0,
        "method_entered", method="orchestrate", caller=AGENT,
        payload=ev(kind="method_entered", method="orchestrate", caller=AGENT,
                   args=[]))
    ins(c, TID, CAN_A, 1, 0, 1, t0+tick,
        "note", payload=ev(kind="note", label="orchestrate:enter"))

    # === Reject type 1: candid Err note (rollback)
    # A does its work, snapshots state, then emits a :rollback note.
    pre = cbor_encode({"phase": "candidate", "ok": False})
    ins(c, TID, CAN_A, 2, 1, 1, t0+2*tick,
        "state_snapshot", snap_key="status",
        payload=ev(kind="state_snapshot", key="status", cbor=list(pre)))
    snap_ins(c, TID, CAN_A, 2, "status", pre)
    ins(c, TID, CAN_A, 3, 2, 1, t0+3*tick,
        "note", payload=ev(kind="note", label="orchestrate:rollback_missing"))

    # === Reject type 2: IC-level call reject (call_returned with reject)
    ins(c, TID, CAN_A, 4, 3, 1, t0+4*tick,
        "call_spawned", target=CAN_B, method="rejecting_method",
        payload=ev(kind="call_spawned", target=CAN_B, method="rejecting_method",
                   args_hash="rj01"))

    ins(c, TID, CAN_B, 0, 4, 2, t0+5*tick,
        "method_entered", method="rejecting_method", caller=CAN_A,
        payload=ev(kind="method_entered", method="rejecting_method", caller=CAN_A,
                   args=[]))
    # B exits with a reject
    ins(c, TID, CAN_B, 1, 0, 2, t0+6*tick,
        "method_exited", reject="canister rejected: simulated IC-level reject",
        payload=ev(kind="method_exited",
                   reject="canister rejected: simulated IC-level reject"))

    # A sees the reject on call_returned
    ins(c, TID, CAN_A, 5, 4, 1, t0+7*tick,
        "call_returned",
        reject="canister rejected: simulated IC-level reject",
        payload=ev(kind="call_returned",
                   reject="canister rejected: simulated IC-level reject"))

    # === Reject type 3: trap (method_exited with reject from a trap)
    ins(c, TID, CAN_A, 6, 5, 1, t0+8*tick,
        "call_spawned", target=CAN_C, method="trap_method",
        payload=ev(kind="call_spawned", target=CAN_C, method="trap_method",
                   args_hash="tp01"))
    ins(c, TID, CAN_C, 0, 6, 3, t0+9*tick,
        "method_entered", method="trap_method", caller=CAN_A,
        payload=ev(kind="method_entered", method="trap_method", caller=CAN_A,
                   args=[]))
    ins(c, TID, CAN_C, 1, 0, 3, t0+10*tick,
        "note", payload=ev(kind="note", label="trap_method:trapped"))
    ins(c, TID, CAN_C, 2, 1, 3, t0+11*tick,
        "method_exited",
        reject="canister trapped: deliberate test trap",
        payload=ev(kind="method_exited",
                   reject="canister trapped: deliberate test trap"))

    ins(c, TID, CAN_A, 7, 6, 1, t0+12*tick,
        "call_returned",
        reject="canister trapped: deliberate test trap",
        payload=ev(kind="call_returned",
                   reject="canister trapped: deliberate test trap"))

    # A finally exits cleanly (orchestrator survived all three failures).
    ins(c, TID, CAN_A, 8, 7, 1, t0+13*tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))


# ── Trace 5: many_keys ───────────────────────────────────────────────────────

def build_many_keys(c, t0):
    TID = TRACE_IDS["many_keys"]
    trace_row(c, TID, t0, TRACE_LABELS["many_keys"])
    tick = 1_000_000

    N_KEYS = 25
    ins(c, TID, CAN_A, 0, None, 1, t0,
        "method_entered", method="multikey", caller=AGENT,
        payload=ev(kind="method_entered", method="multikey", caller=AGENT,
                   args=[]))
    ins(c, TID, CAN_A, 1, 0, 1, t0+tick,
        "note", payload=ev(kind="note", label="multikey:enter"))

    seq = 2
    # Each key gets two snapshots so the diff engine produces a transition.
    for i in range(N_KEYS):
        key = f"key_{i:02d}"
        ts1 = t0 + (2 + i*4) * tick
        ts2 = ts1 + 2*tick

        v1 = cbor_encode({"value": i, "label": "before"})
        v2 = cbor_encode({"value": i*10, "label": "after"})

        ins(c, TID, CAN_A, seq, seq-1, 1, ts1,
            "state_snapshot", snap_key=key,
            payload=ev(kind="state_snapshot", key=key, cbor=list(v1)))
        snap_ins(c, TID, CAN_A, seq, key, v1)
        seq += 1

        ins(c, TID, CAN_A, seq, seq-1, 1, ts2,
            "state_snapshot", snap_key=key,
            payload=ev(kind="state_snapshot", key=key, cbor=list(v2)))
        snap_ins(c, TID, CAN_A, seq, key, v2)
        seq += 1

    ins(c, TID, CAN_A, seq, seq-1, 1, t0 + (2 + N_KEYS*4 + 1) * tick,
        "method_exited", payload=ev(kind="method_exited", reject=None))


# ── Main ─────────────────────────────────────────────────────────────────────

def main():
    if os.path.exists(DB):
        os.remove(DB)
    conn = sqlite3.connect(DB)
    try:
        conn.executescript(SCHEMA)
        c = conn.cursor()
        for principal, name in NAMES.items():
            c.execute("INSERT OR REPLACE INTO canister_names VALUES (?,?)",
                      (principal, name))
        t0 = int(time.time() * 1_000_000_000)
        build_deep_chain(c, t0)
        build_fan_out(c, t0 + 100_000_000_000)
        build_high_volume(c, t0 + 200_000_000_000)
        build_mixed_rejects(c, t0 + 300_000_000_000)
        build_many_keys(c, t0 + 400_000_000_000)
        conn.commit()

        # Summary counts
        n_traces = c.execute("SELECT COUNT(*) FROM traces").fetchone()[0]
        n_events = c.execute("SELECT COUNT(*) FROM events").fetchone()[0]
        n_snaps  = c.execute("SELECT COUNT(*) FROM snapshots").fetchone()[0]
        print(f"Wrote {n_traces} traces, {n_events} events, {n_snaps} snapshots")
        print(f"DB: {DB}")
    finally:
        conn.close()


if __name__ == "__main__":
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except Exception:
        pass
    main()
