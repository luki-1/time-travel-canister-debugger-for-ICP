//! Replay engine — load a recorded trace from SQLite, reconstruct the global
//! causal order across canisters, and render a time-travel walkthrough.
//!
//! Ordering rule: events are per-canister (each canister owns its `seq`
//! counter), so the global order is by `ts_nanos`, with `(canister, seq)`
//! as the deterministic tie-break. The `parent_seq` chain links an event
//! to its direct causal predecessor within the same canister; at trace
//! entry (`begin_trace`) it's seeded from the inbound `TraceHeader` so
//! cross-canister edges survive the hop.
//!
//! This is a deterministic replay of the recorded stream — same trace in,
//! same output out. Live re-execution against a fresh replica is a
//! follow-up once we start recording full ingress args.

use anyhow::{Context, Result};
use ic_debug_core::EventKind;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};

pub struct Options {
    pub store: String,
    pub trace: String,
    pub step: bool,
    pub decode_cbor: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
pub struct TraceSummary {
    pub trace_id: String,
    pub started_at: i64,
    pub label: String,
    pub event_count: usize,
    pub canisters: Vec<String>,
    pub duration_nanos: u128,
    pub call_spawned: usize,
    pub rejects: usize,
}

#[derive(Debug, Serialize)]
pub struct EventRow {
    pub idx: usize,
    pub canister: String,
    pub seq: u64,
    pub parent_seq: Option<u64>,
    pub span_id: u64,
    pub ts_nanos: u128,
    pub recv_nanos: Option<u128>,
    pub kind: EventKind,
}

pub fn run(opts: Options) -> Result<()> {
    let conn = Connection::open(&opts.store)
        .with_context(|| format!("open sqlite store {}", opts.store))?;

    let (summary, events) = load(&conn, &opts.trace)?;
    if events.is_empty() {
        anyhow::bail!("no events recorded for trace {}", opts.trace);
    }

    if opts.json {
        let out = serde_json::json!({
            "summary": summary,
            "events": events,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    render_header(&summary);
    render_timeline(&events, opts.step, opts.decode_cbor)?;
    render_footer(&summary);
    Ok(())
}

pub fn load(conn: &Connection, trace_id: &str) -> Result<(TraceSummary, Vec<EventRow>)> {
    // Summary row
    let (started_at, label): (i64, String) = conn
        .query_row(
            "SELECT started_at, label FROM traces WHERE id = ?1",
            [trace_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, String::new()));

    let mut stmt = conn.prepare(
        "SELECT canister, seq, parent_seq, span_id, ts_nanos, recv_nanos, payload_json
           FROM events
          WHERE trace_id = ?1
          ORDER BY CAST(ts_nanos AS INTEGER), canister, seq",
    )?;

    let rows = stmt.query_map([trace_id], |r| {
        let canister: String = r.get(0)?;
        let seq: i64 = r.get(1)?;
        let parent_seq: Option<i64> = r.get(2)?;
        let span_id: i64 = r.get(3)?;
        let ts_nanos_s: String = r.get(4)?;
        let recv_nanos_s: Option<String> = r.get(5)?;
        let payload: String = r.get(6)?;
        Ok((canister, seq, parent_seq, span_id, ts_nanos_s, recv_nanos_s, payload))
    })?;

    let mut events = Vec::new();
    for r in rows {
        let (canister, seq, parent_seq, span_id, ts_nanos_s, recv_nanos_s, payload) = r?;
        let ts_nanos: u128 = ts_nanos_s
            .parse()
            .with_context(|| format!("parse ts_nanos {ts_nanos_s}"))?;
        let recv_nanos: Option<u128> = match recv_nanos_s {
            Some(s) => Some(s.parse().with_context(|| format!("parse recv_nanos {s}"))?),
            None => None,
        };
        let kind: EventKind = serde_json::from_str(&payload)
            .with_context(|| format!("decode kind json for event seq {seq}"))?;
        events.push(EventRow {
            idx: 0, // assigned after causal reorder
            canister,
            seq: seq as u64,
            parent_seq: parent_seq.map(|v| v as u64),
            span_id: span_id as u64,
            ts_nanos,
            recv_nanos,
            kind,
        });
    }

    let mut events = reorder_causal(events);
    for (i, e) in events.iter_mut().enumerate() {
        e.idx = i + 1;
    }

    let mut canisters = Vec::new();
    for e in &events {
        if !canisters.contains(&e.canister) {
            canisters.push(e.canister.clone());
        }
    }
    let duration_nanos = events
        .last()
        .map(|e| e.ts_nanos - events.first().unwrap().ts_nanos)
        .unwrap_or(0);
    let call_spawned = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::CallSpawned { .. }))
        .count();
    let rejects = events
        .iter()
        .filter(|e| matches!(&e.kind,
            EventKind::CallReturned { reject: Some(_) } |
            EventKind::MethodExited { reject: Some(_) }))
        .count();

    let summary = TraceSummary {
        trace_id: trace_id.to_string(),
        started_at,
        label,
        event_count: events.len(),
        canisters,
        duration_nanos,
        call_spawned,
        rejects,
    };
    Ok((summary, events))
}

/// Re-order events so the timeline reads as a single interleaved narrative:
/// start from the root canister, and whenever we hit a `CallSpawned` splice
/// in the callee's events. On local replica all `ts_nanos` collapse to the
/// same block tick, so raw timestamp-sort would cluster by canister and
/// destroy the story — hence this explicit splice.
fn reorder_causal(events: Vec<EventRow>) -> Vec<EventRow> {
    // Group events per canister, sorted by seq.
    let mut by_can: BTreeMap<String, Vec<EventRow>> = BTreeMap::new();
    for e in events {
        by_can.entry(e.canister.clone()).or_default().push(e);
    }
    for v in by_can.values_mut() {
        v.sort_by_key(|e| e.seq);
    }

    // The root is the canister whose first event has no cross-canister
    // parent — parent_seq is either None or zero.
    let root = by_can
        .iter()
        .find(|(_, es)| {
            es.first()
                .map(|e| matches!(e.parent_seq, None | Some(0)))
                .unwrap_or(false)
        })
        .map(|(k, _)| k.clone());

    // Fallback: no root found, return time-sorted flatten.
    let Some(root) = root else {
        let mut flat: Vec<EventRow> = by_can.into_values().flatten().collect();
        flat.sort_by_key(|e| (e.ts_nanos, e.canister.clone(), e.seq));
        return flat;
    };

    let mut out = Vec::new();
    splice_canister(&root, &mut by_can, &mut out);
    // Append any orphan canister sessions we never matched, so no data is lost.
    for (_, rest) in by_can {
        out.extend(rest);
    }
    out
}

/// Depth-first walk: emit this canister's events in seq order; at each
/// `CallSpawned(target=T)` recursively splice T's events.
fn splice_canister(
    can: &str,
    by_can: &mut BTreeMap<String, Vec<EventRow>>,
    out: &mut Vec<EventRow>,
) {
    let Some(events) = by_can.remove(can) else { return };
    for e in events {
        let target = if let EventKind::CallSpawned { target, .. } = &e.kind {
            Some(target.clone())
        } else {
            None
        };
        out.push(e);
        if let Some(t) = target {
            if by_can.contains_key(&t) {
                splice_canister(&t, by_can, out);
            }
        }
    }
}

fn render_header(s: &TraceSummary) {
    println!("═══ trace {} ═══", s.trace_id);
    if !s.label.is_empty() {
        println!("  label:     {}", s.label);
    }
    println!("  events:    {}", s.event_count);
    println!("  duration:  {} µs", s.duration_nanos / 1_000);
    println!("  canisters: {}", s.canisters.len());
    for c in &s.canisters {
        println!("    · {c}");
    }
    println!("  calls:     {}   rejects: {}", s.call_spawned, s.rejects);
    println!();
}

fn render_footer(s: &TraceSummary) {
    println!();
    if s.rejects > 0 {
        println!("⚠ {} reject event(s) observed — see entries marked ✗ above.", s.rejects);
    } else {
        println!("✓ replay complete, no rejects observed.");
    }
}

fn render_timeline(events: &[EventRow], step: bool, decode_cbor: bool) -> Result<()> {
    // Per-canister depth driven by MethodEntered / MethodExited.
    let mut depth: HashMap<String, usize> = HashMap::new();
    // Short tag per canister: first label before the first dash.
    let tag: HashMap<String, String> = {
        let mut m = HashMap::new();
        for e in events {
            m.entry(e.canister.clone())
                .or_insert_with(|| short_principal(&e.canister));
        }
        m
    };

    let t0 = events.first().map(|e| e.ts_nanos).unwrap_or(0);

    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();

    for e in events {
        let dt = e.ts_nanos - t0;
        let t = tag.get(&e.canister).cloned().unwrap_or_default();

        // Pop depth before printing an exit so the marker aligns with its entry.
        if matches!(e.kind, EventKind::MethodExited { .. }) {
            let d = depth.get(&e.canister).copied().unwrap_or(1);
            depth.insert(e.canister.clone(), d.saturating_sub(1));
        }
        let d = *depth.get(&e.canister).unwrap_or(&0);
        let indent = "│ ".repeat(d);

        let parent = e
            .parent_seq
            .map(|p| format!("←{p}"))
            .unwrap_or_else(|| "  ".to_string());

        print!(
            "#{idx:03} +{dt:>6}µs [{t:<5}] seq={seq:<2} {parent:<4} {indent}",
            idx = e.idx,
            dt = dt / 1_000,
            t = t,
            seq = e.seq,
            parent = parent,
            indent = indent,
        );
        render_kind(&e.kind, decode_cbor);

        // Push depth after printing an entry.
        if matches!(e.kind, EventKind::MethodEntered { .. } | EventKind::IngressEntered { .. }) {
            let d = depth.get(&e.canister).copied().unwrap_or(0);
            depth.insert(e.canister.clone(), d + 1);
        }

        if step {
            print!("      » enter=advance, q=quit ");
            io::stdout().flush()?;
            let mut line = String::new();
            stdin_lock.read_line(&mut line)?;
            if line.trim().eq_ignore_ascii_case("q") {
                println!("  … replay aborted.");
                break;
            }
        }
    }
    Ok(())
}

fn render_kind(k: &EventKind, decode_cbor: bool) {
    match k {
        EventKind::IngressEntered { method, caller, .. } => {
            println!("▶ INGRESS {method}  caller={}", short_principal(caller));
        }
        EventKind::MethodEntered { method, caller, args } => {
            let rendered = if args.is_empty() {
                String::new()
            } else {
                let joined = args
                    .iter()
                    .map(|(n, v)| format!("{n}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({joined})")
            };
            println!(
                "▶ ENTER   {method}{rendered}  caller={}",
                short_principal(caller)
            );
        }
        EventKind::MethodExited { reject } => match reject {
            None => println!("◀ EXIT"),
            Some(r) => println!("✗ EXIT    reject={r}"),
        },
        EventKind::CallSpawned { target, method, args_hash } => {
            println!(
                "→ CALL    {method} on {}  args_hash={}",
                short_principal(target),
                hex8(args_hash)
            );
        }
        EventKind::CallReturned { reject } => match reject {
            None => println!("← RET     ok"),
            Some(r) => println!("✗ RET     reject={r}"),
        },
        EventKind::StateSnapshot { key, cbor } => {
            if decode_cbor {
                match decode_cbor_value(cbor) {
                    Ok(s) => println!("• STATE   {key} = {s}"),
                    Err(_) => println!("• STATE   {key}  ({} bytes cbor, undecodable)", cbor.len()),
                }
            } else {
                println!("• STATE   {key}  ({} bytes cbor)", cbor.len());
            }
        }
        EventKind::TimerFired { label } => println!("⏰ TIMER  {label}"),
        EventKind::Note { label } => println!("· NOTE    {label}"),
    }
}

fn short_principal(s: &str) -> String {
    match s.find('-') {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn hex8(bytes: &[u8]) -> String {
    let n = bytes.len().min(4);
    let mut s = String::with_capacity(2 * n + 1);
    for b in &bytes[..n] {
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > n {
        s.push('…');
    }
    s
}

fn decode_cbor_value(cbor: &[u8]) -> Result<String> {
    let v: ciborium::Value = ciborium::from_reader(cbor)?;
    Ok(format_cbor(&v))
}

fn format_cbor(v: &ciborium::Value) -> String {
    use ciborium::Value as V;
    match v {
        V::Integer(i) => {
            let n: i128 = (*i).into();
            n.to_string()
        }
        V::Bytes(b) => format!("b\"{}\"", hex8(b)),
        V::Float(f) => f.to_string(),
        V::Text(t) => format!("\"{t}\""),
        V::Bool(b) => b.to_string(),
        V::Null => "null".to_string(),
        V::Tag(_, inner) => format_cbor(inner),
        V::Array(items) => {
            let parts: Vec<String> = items.iter().map(format_cbor).collect();
            format!("[{}]", parts.join(", "))
        }
        V::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}: {}", format_cbor(k), format_cbor(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        _ => "?".to_string(),
    }
}
