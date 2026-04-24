//! State-diff engine over CBOR snapshots recorded by `trace_state!`.
//!
//! Each `StateSnapshot { key, cbor }` lands in the `snapshots` SQLite table
//! as `(trace_id, canister, seq, key, cbor)`. Within a single canister, seq
//! orders time. For a given (canister, key) pair we line the snapshots up
//! in seq order and compute a structural delta between each consecutive
//! pair: fields added, removed, or changed.

use anyhow::{anyhow, Context, Result};
use ciborium::Value;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub struct Options {
    pub store: String,
    pub trace: String,
    /// Scope to a single canister (principal text).
    pub canister: Option<String>,
    /// Scope to a single snapshot key (e.g. "payment").
    pub key: Option<String>,
    /// If both `from` and `to` are set, diff those two snapshots by seq
    /// (requires `canister` + `key` so the pair is unambiguous).
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub json: bool,
}

#[derive(Debug, Serialize)]
pub enum Delta {
    Added { path: String, value: String },
    Removed { path: String, value: String },
    Changed { path: String, from: String, to: String },
}

#[derive(Debug, Serialize)]
pub struct Transition {
    pub canister: String,
    pub key: String,
    pub from_seq: u64,
    pub to_seq: u64,
    pub deltas: Vec<Delta>,
}

#[derive(Debug, Serialize)]
pub struct Initial {
    pub canister: String,
    pub key: String,
    pub seq: u64,
    pub value: String,
}

pub struct Snapshot {
    pub canister: String,
    pub seq: u64,
    pub key: String,
    pub cbor: Vec<u8>,
}

/// Convenience for the HTTP serve layer: run the full walk (load + group +
/// build) in one shot.
pub fn walk_from_store(
    conn: &Connection,
    trace_id: &str,
) -> Result<(Vec<Transition>, Vec<Initial>)> {
    let snapshots = load_snapshots(conn, trace_id, None, None)?;
    let grouped = group_by_ck(snapshots);
    build_walk(&grouped)
}

pub fn run(opts: Options) -> Result<()> {
    let conn = Connection::open(&opts.store)
        .with_context(|| format!("open sqlite store {}", opts.store))?;

    let snapshots = load_snapshots(
        &conn,
        &opts.trace,
        opts.canister.as_deref(),
        opts.key.as_deref(),
    )?;

    if snapshots.is_empty() {
        println!(
            "no state snapshots recorded for trace {} with the given filters",
            opts.trace
        );
        return Ok(());
    }

    // Specific pair mode.
    if opts.from.is_some() || opts.to.is_some() {
        let from = opts.from.ok_or_else(|| anyhow!("--from required when --to is set"))?;
        let to = opts.to.ok_or_else(|| anyhow!("--to required when --from is set"))?;
        return run_pair(&snapshots, from, to, &opts);
    }

    // Walk mode: group by (canister, key) and show every transition.
    let grouped = group_by_ck(snapshots);
    let (transitions, initials) = build_walk(&grouped)?;

    if opts.json {
        let out = serde_json::json!({
            "trace_id": opts.trace,
            "transitions": transitions,
            "initials": initials,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    render_walk(&opts.trace, &grouped, &transitions, &initials);
    Ok(())
}

fn run_pair(
    snapshots: &[Snapshot],
    from: u64,
    to: u64,
    opts: &Options,
) -> Result<()> {
    // Pair mode needs both --canister and --key so the lookup is unique
    // (snapshots share seq space per-canister, not globally).
    if opts.canister.is_none() || opts.key.is_none() {
        return Err(anyhow!(
            "--from/--to requires --canister and --key to disambiguate the snapshot pair"
        ));
    }
    let a = snapshots
        .iter()
        .find(|s| s.seq == from)
        .ok_or_else(|| anyhow!("no snapshot with seq={from} for that canister+key"))?;
    let b = snapshots
        .iter()
        .find(|s| s.seq == to)
        .ok_or_else(|| anyhow!("no snapshot with seq={to} for that canister+key"))?;

    let av = decode(&a.cbor)?;
    let bv = decode(&b.cbor)?;
    let deltas = diff_values(&av, &bv);
    let t = Transition {
        canister: a.canister.clone(),
        key: a.key.clone(),
        from_seq: from,
        to_seq: to,
        deltas,
    };

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&t)?);
    } else {
        render_transition(&t);
    }
    Ok(())
}

fn load_snapshots(
    conn: &Connection,
    trace_id: &str,
    canister: Option<&str>,
    key: Option<&str>,
) -> Result<Vec<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT canister, seq, key, cbor
           FROM snapshots
          WHERE trace_id = ?1
          ORDER BY canister, key, seq",
    )?;
    let rows = stmt.query_map([trace_id], |r| {
        Ok(Snapshot {
            canister: r.get(0)?,
            seq: r.get::<_, i64>(1)? as u64,
            key: r.get(2)?,
            cbor: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for s in rows {
        let s = s?;
        if let Some(c) = canister {
            if s.canister != c {
                continue;
            }
        }
        if let Some(k) = key {
            if s.key != k {
                continue;
            }
        }
        out.push(s);
    }
    Ok(out)
}

fn group_by_ck(
    snapshots: Vec<Snapshot>,
) -> BTreeMap<(String, String), Vec<Snapshot>> {
    let mut out: BTreeMap<(String, String), Vec<Snapshot>> = BTreeMap::new();
    for s in snapshots {
        out.entry((s.canister.clone(), s.key.clone()))
            .or_default()
            .push(s);
    }
    // seq-sort within each group (load query already ordered, but be safe).
    for v in out.values_mut() {
        v.sort_by_key(|s| s.seq);
    }
    out
}

fn build_walk(
    grouped: &BTreeMap<(String, String), Vec<Snapshot>>,
) -> Result<(Vec<Transition>, Vec<Initial>)> {
    let mut transitions = Vec::new();
    let mut initials = Vec::new();
    for ((canister, key), snaps) in grouped {
        if snaps.len() == 1 {
            let v = decode(&snaps[0].cbor)?;
            initials.push(Initial {
                canister: canister.clone(),
                key: key.clone(),
                seq: snaps[0].seq,
                value: format_value(&v),
            });
            continue;
        }
        for pair in snaps.windows(2) {
            let a = decode(&pair[0].cbor)?;
            let b = decode(&pair[1].cbor)?;
            let deltas = diff_values(&a, &b);
            transitions.push(Transition {
                canister: canister.clone(),
                key: key.clone(),
                from_seq: pair[0].seq,
                to_seq: pair[1].seq,
                deltas,
            });
        }
    }
    Ok((transitions, initials))
}

// -------- diff core --------

fn diff_values(a: &Value, b: &Value) -> Vec<Delta> {
    let mut out = Vec::new();
    diff_rec("", a, b, &mut out);
    out
}

fn diff_rec(path: &str, a: &Value, b: &Value, out: &mut Vec<Delta>) {
    match (a, b) {
        (Value::Map(am), Value::Map(bm)) => {
            // CBOR map keys can be any Value; for struct-like data they
            // are text. Treat non-text keys as opaque via `format_value`.
            let mut aa: BTreeMap<String, &Value> = BTreeMap::new();
            for (k, v) in am {
                aa.insert(format_value(k), v);
            }
            let mut bb: BTreeMap<String, &Value> = BTreeMap::new();
            for (k, v) in bm {
                bb.insert(format_value(k), v);
            }
            let mut keys: BTreeSet<&String> = BTreeSet::new();
            keys.extend(aa.keys());
            keys.extend(bb.keys());
            for k in keys {
                let kp = strip_quotes(k);
                let child = if path.is_empty() {
                    format!("/{kp}")
                } else {
                    format!("{path}/{kp}")
                };
                match (aa.get(k), bb.get(k)) {
                    (Some(av), Some(bv)) => diff_rec(&child, av, bv, out),
                    (Some(av), None) => out.push(Delta::Removed {
                        path: child,
                        value: format_value(av),
                    }),
                    (None, Some(bv)) => out.push(Delta::Added {
                        path: child,
                        value: format_value(bv),
                    }),
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Array(av), Value::Array(bv)) => {
            let n = av.len().max(bv.len());
            for i in 0..n {
                let child = if path.is_empty() {
                    format!("/{i}")
                } else {
                    format!("{path}/{i}")
                };
                match (av.get(i), bv.get(i)) {
                    (Some(a), Some(b)) => diff_rec(&child, a, b, out),
                    (Some(a), None) => out.push(Delta::Removed {
                        path: child,
                        value: format_value(a),
                    }),
                    (None, Some(b)) => out.push(Delta::Added {
                        path: child,
                        value: format_value(b),
                    }),
                    (None, None) => unreachable!(),
                }
            }
        }
        _ => {
            if !values_equal(a, b) {
                out.push(Delta::Changed {
                    path: if path.is_empty() { "/".to_string() } else { path.to_string() },
                    from: format_value(a),
                    to: format_value(b),
                });
            }
        }
    }
}

/// ciborium::Value doesn't implement PartialEq in every case we care about,
/// so compare via canonical serialized form.
fn values_equal(a: &Value, b: &Value) -> bool {
    let mut ba = Vec::new();
    let mut bb = Vec::new();
    let ra = ciborium::into_writer(a, &mut ba);
    let rb = ciborium::into_writer(b, &mut bb);
    ra.is_ok() && rb.is_ok() && ba == bb
}

// -------- CBOR helpers --------

fn decode(cbor: &[u8]) -> Result<Value> {
    ciborium::from_reader(cbor).context("decode CBOR snapshot")
}

pub fn format_value(v: &Value) -> String {
    match v {
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            n.to_string()
        }
        Value::Bytes(b) => format!("b\"{}\"", hex_short(b)),
        Value::Float(f) => f.to_string(),
        Value::Text(t) => format!("\"{t}\""),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Tag(_, inner) => format_value(inner),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(format_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}: {}", format_value(k), format_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        _ => "?".to_string(),
    }
}

fn hex_short(bytes: &[u8]) -> String {
    let n = bytes.len().min(8);
    let mut s = String::with_capacity(2 * n + 1);
    for b in &bytes[..n] {
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > n {
        s.push('…');
    }
    s
}

fn strip_quotes(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

// -------- rendering --------

fn short_principal(s: &str) -> String {
    match s.find('-') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn render_walk(
    trace_id: &str,
    grouped: &BTreeMap<(String, String), Vec<Snapshot>>,
    transitions: &[Transition],
    initials: &[Initial],
) {
    println!("═══ state transitions in trace {trace_id} ═══");
    println!(
        "  {} snapshot group(s) · {} transition(s) · {} single-snapshot(s)",
        grouped.len(),
        transitions.len(),
        initials.len()
    );
    println!();

    // Print transitions in the same grouping order as the map.
    let mut by_group: BTreeMap<(String, String), Vec<&Transition>> = BTreeMap::new();
    for t in transitions {
        by_group
            .entry((t.canister.clone(), t.key.clone()))
            .or_default()
            .push(t);
    }

    for ((canister, key), snaps) in grouped {
        let tag = short_principal(canister);
        println!("[{tag}] {key}  ({} snapshot(s))", snaps.len());
        if let Some(ts) = by_group.get(&(canister.clone(), key.clone())) {
            for t in ts {
                render_transition(t);
            }
        } else if let Some(init) = initials
            .iter()
            .find(|i| i.canister == *canister && i.key == *key)
        {
            println!("  · initial @ seq={}: {}", init.seq, init.value);
        }
        println!();
    }
}

fn render_transition(t: &Transition) {
    if t.deltas.is_empty() {
        println!(
            "  seq {} → {}: (no structural change)",
            t.from_seq, t.to_seq
        );
        return;
    }
    println!("  seq {} → {}:", t.from_seq, t.to_seq);
    for d in &t.deltas {
        match d {
            Delta::Added { path, value } => println!("    + {path} = {value}"),
            Delta::Removed { path, value } => println!("    - {path} = {value}"),
            Delta::Changed { path, from, to } => println!("    ~ {path}: {from} → {to}"),
        }
    }
}
