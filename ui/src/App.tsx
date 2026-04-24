import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import "./App.css";

// -------- Shared types --------
type EventKind =
  | { kind: "method_entered"; method: string; caller: string; args?: [string, string][] }
  | { kind: "ingress_entered"; method: string; caller: string; args_hash: number[] }
  | { kind: "method_exited"; reject: string | null }
  | { kind: "call_spawned"; target: string; method: string; args_hash: number[] }
  | { kind: "call_returned"; reject: string | null }
  | { kind: "state_snapshot"; key: string; cbor: number[] }
  | { kind: "note"; label: string }
  | { kind: "timer_fired"; label: string };

type EventRow = {
  idx: number;
  canister: string;
  seq: number;
  parent_seq: number | null;
  span_id: number;
  ts_nanos: number;
  kind: EventKind;
};

type Summary = {
  trace_id: string;
  started_at: number;
  label: string;
  event_count: number;
  canisters: string[];
  duration_nanos: number;
  call_spawned: number;
  rejects: number;
};

type TraceRow = {
  trace_id: string;
  started_at: number;
  label: string;
  event_count: number;
};

type Trace = { summary: Summary; events: EventRow[] };

type CanisterName = {
  principal: string;
  name: string;
  source: "override" | "default";
  default_name: string | null;
};

type NameMap = Record<string, CanisterName>;

type Delta =
  | { Added: { path: string; value: string } }
  | { Removed: { path: string; value: string } }
  | { Changed: { path: string; from: string; to: string } };

type Transition = {
  canister: string;
  key: string;
  from_seq: number;
  to_seq: number;
  deltas: Delta[];
};

type Initial = { canister: string; key: string; seq: number; value: string };

type DiffDoc = {
  trace_id: string;
  transitions: Transition[];
  initials: Initial[];
};

// -------- Helpers --------
const shortPrincipal = (p: string) => {
  const i = p.indexOf("-");
  return i > 0 ? p.slice(0, i) : p;
};

// Short label for a canister: the user-set or default name if we have one,
// otherwise the principal prefix. Passed through the tree so every place
// that used to call `shortPrincipal` goes through one source of truth.
const makeCanisterLabel =
  (names: NameMap) =>
  (p: string): string =>
    names[p]?.name ?? shortPrincipal(p);

const canisterColor = (p: string) => {
  let h = 0;
  for (let i = 0; i < p.length; i++) h = (h * 31 + p.charCodeAt(i)) & 0xffffff;
  const hue = ((h % 360) + 360) % 360;
  return `hsl(${hue}, 55%, 60%)`;
};

const kindGlyph = (k: EventKind): string => {
  switch (k.kind) {
    case "method_entered":
    case "ingress_entered":
      return "▶";
    case "method_exited":
      return k.reject ? "✗" : "◀";
    case "call_spawned":
      return "→";
    case "call_returned":
      return k.reject ? "✗" : "←";
    case "state_snapshot":
      return "●";
    case "note":
      return "·";
    case "timer_fired":
      return "⏰";
  }
};

const kindSummary = (k: EventKind, labelFor: (p: string) => string): string => {
  switch (k.kind) {
    case "method_entered": {
      const args = (k.args ?? [])
        .map(([name, value]) => `${name}=${value}`)
        .join(", ");
      const argStr = args ? `(${args})` : "()";
      return `ENTER  ${k.method}${argStr}   caller=${labelFor(k.caller)}`;
    }
    case "ingress_entered":
      return `INGRESS ${k.method}  caller=${labelFor(k.caller)}`;
    case "method_exited":
      return k.reject ? `EXIT   reject=${k.reject}` : `EXIT`;
    case "call_spawned":
      return `CALL   ${k.method} on ${labelFor(k.target)}`;
    case "call_returned":
      return k.reject ? `RET    reject=${k.reject}` : `RET    ok`;
    case "state_snapshot":
      return `STATE  ${k.key}`;
    case "note":
      return `NOTE   ${k.label}`;
    case "timer_fired":
      return `TIMER  ${k.label}`;
  }
};

// -------- Error flagging --------
const FLAG_WORDS = /fail|reject|rollback/i;

const isFlagged = (e: EventRow): boolean => {
  const k = e.kind;
  if ((k.kind === "method_exited" || k.kind === "call_returned") && k.reject) return true;
  if (k.kind === "note" && FLAG_WORDS.test(k.label)) return true;
  return false;
};

const flagReason = (e: EventRow): string | null => {
  const k = e.kind;
  if ((k.kind === "method_exited" || k.kind === "call_returned") && k.reject) return k.reject;
  if (k.kind === "note" && FLAG_WORDS.test(k.label)) return k.label;
  return null;
};

// -------- Root --------
export default function App() {
  const [traces, setTraces] = useState<TraceRow[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [trace, setTrace] = useState<Trace | null>(null);
  const [diff, setDiff] = useState<DiffDoc | null>(null);
  const [cursor, setCursor] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [names, setNames] = useState<NameMap>({});
  const labelFor = useMemo(() => makeCanisterLabel(names), [names]);

  // Load default canister names from the server (mapping files + user
  // overrides) on mount. Failures are non-fatal — we just fall back to
  // principal prefixes.
  const refreshNames = useCallback(async () => {
    try {
      const rows: CanisterName[] = await fetch("/api/canisters").then((r) =>
        r.json()
      );
      const map: NameMap = {};
      for (const row of rows) map[row.principal] = row;
      setNames(map);
    } catch (e) {
      // leave names empty; principals will still render.
      console.warn("load canister names:", e);
    }
  }, []);

  // Save or clear an override. Called by the rename UI.
  const renameCanister = useCallback(
    async (principal: string, name: string | null) => {
      try {
        if (name === null) {
          await fetch(`/api/canisters/${encodeURIComponent(principal)}`, {
            method: "DELETE",
          });
        } else {
          const trimmed = name.trim();
          if (!trimmed) return;
          await fetch(`/api/canisters/${encodeURIComponent(principal)}`, {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ name: trimmed }),
          });
        }
        await refreshNames();
      } catch (e) {
        setError(`rename canister: ${e}`);
      }
    },
    [refreshNames]
  );

  useEffect(() => {
    void refreshNames();
  }, [refreshNames]);

  const flaggedIdxs = useMemo(
    () =>
      trace
        ? trace.events.reduce<number[]>((a, e, i) => {
            if (isFlagged(e)) a.push(i);
            return a;
          }, [])
        : [],
    [trace]
  );

  // ---- timeline feature state ----
  const [hiddenCanisters, setHiddenCanisters] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set());
  const [showTimestamps, setShowTimestamps] = useState(false);
  const [search, setSearch] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [stateMode, setStateMode] = useState<"history" | "current">("current");

  const toggleCanisterVisible = useCallback((p: string) => {
    setHiddenCanisters((prev) => {
      const next = new Set(prev);
      if (next.has(p)) next.delete(p);
      else next.add(p);
      return next;
    });
  }, []);

  const toggleCollapse = useCallback((i: number) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  // Maps each method_entered index → its matching method_exited index.
  const exitForEnter = useMemo(() => {
    if (!trace) return new Map<number, number>();
    const map = new Map<number, number>();
    const stacks = new Map<string, number[]>();
    trace.events.forEach((e, i) => {
      if (e.kind.kind === "method_entered" || e.kind.kind === "ingress_entered") {
        if (!stacks.has(e.canister)) stacks.set(e.canister, []);
        stacks.get(e.canister)!.push(i);
      } else if (e.kind.kind === "method_exited") {
        const stack = stacks.get(e.canister);
        if (stack && stack.length > 0) map.set(stack.pop()!, i);
      }
    });
    return map;
  }, [trace]);

  const hiddenByCollapse = useMemo(() => {
    const hidden = new Set<number>();
    for (const [enterIdx, exitIdx] of exitForEnter) {
      if (collapsed.has(enterIdx)) {
        for (let i = enterIdx + 1; i <= exitIdx; i++) hidden.add(i);
      }
    }
    return hidden;
  }, [exitForEnter, collapsed]);

  const visibleIndices = useMemo(
    () =>
      trace
        ? trace.events
            .map((_, i) => i)
            .filter(
              (i) =>
                !hiddenCanisters.has(trace.events[i].canister) &&
                !hiddenByCollapse.has(i)
            )
        : [],
    [trace, hiddenCanisters, hiddenByCollapse]
  );

  const searchMatches = useMemo(() => {
    if (!search || !trace) return null;
    const q = search.toLowerCase();
    return new Set(
      trace.events
        .map((e, i) => ({ e, i }))
        .filter(({ e }) => kindSummary(e.kind, labelFor).toLowerCase().includes(q))
        .map(({ i }) => i)
    );
  }, [search, trace, labelFor]);

  const traceStartNs = trace?.events[0]?.ts_nanos ?? 0;

  // Load trace list on mount.
  useEffect(() => {
    fetch("/api/traces")
      .then((r) => r.json())
      .then((rows: TraceRow[]) => {
        setTraces(rows);
        if (rows.length > 0) setSelected(rows[0].trace_id);
      })
      .catch((e) => setError(`list traces: ${e}`));
  }, []);

  // Load the selected trace + diff.
  useEffect(() => {
    if (!selected) return;
    setTrace(null);
    setDiff(null);
    setCursor(0);
    setHiddenCanisters(new Set());
    setCollapsed(new Set());
    setSearch("");
    setSearchOpen(false);
    Promise.all([
      fetch(`/api/traces/${selected}`).then((r) => r.json()),
      fetch(`/api/traces/${selected}/diff`).then((r) => r.json()),
    ])
      .then(([t, d]) => {
        setTrace(t);
        setDiff(d);
      })
      .catch((e) => setError(`load trace: ${e}`));
  }, [selected]);

  // Autoplay advancer — advances through visible events only.
  useEffect(() => {
    if (!playing || !trace) return;
    const last = visibleIndices[visibleIndices.length - 1] ?? trace.events.length - 1;
    if (cursor >= last) { setPlaying(false); return; }
    const next = visibleIndices.find((i) => i > cursor) ?? last;
    const id = setTimeout(() => setCursor(next), 500);
    return () => clearTimeout(id);
  }, [playing, cursor, trace, visibleIndices]);

  // Keyboard controls.
  const handleKey = useCallback(
    (e: KeyboardEvent) => {
      if (!trace) return;
      // Let the search input handle its own typing; only intercept Escape.
      if (searchOpen && e.key !== "Escape") return;
      const vis = visibleIndices;
      const nextVis = (c: number) => vis.find((i) => i > c) ?? vis[vis.length - 1] ?? c;
      const prevVis = (c: number) => {
        for (let i = vis.length - 1; i >= 0; i--) if (vis[i] < c) return vis[i];
        return vis[0] ?? c;
      };
      if (e.key === " ") {
        e.preventDefault();
        setPlaying((p) => !p);
      } else if (e.key === "ArrowRight" || e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault();
        setCursor((c) => nextVis(c));
      } else if (e.key === "ArrowLeft" || e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        setCursor((c) => prevVis(c));
      } else if (e.key === "Home") {
        setCursor(vis[0] ?? 0);
      } else if (e.key === "End") {
        setCursor(vis[vis.length - 1] ?? trace.events.length - 1);
      } else if (e.key === "n") {
        const vf = flaggedIdxs.filter((i) => vis.includes(i));
        if (vf.length === 0) return;
        setCursor((c) => vf.find((i) => i > c) ?? vf[0]);
      } else if (e.key === "/") {
        e.preventDefault();
        setSearchOpen(true);
      } else if (e.key === "Escape") {
        setSearchOpen(false);
        setSearch("");
      }
    },
    [trace, flaggedIdxs, visibleIndices, searchOpen]
  );
  useEffect(() => {
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [handleKey]);

  return (
    <div className="app">
      <header className="top-bar">
        <h1>ic-debug</h1>
        <span className="muted">time-travel canister debugger</span>
        <span className="spacer" />
        <span className="muted small">
          ↑↓ step · space play · home/end · n next error · / search
        </span>
      </header>

      {error && <div className="error-bar">{error}</div>}

      <div className="body">
        <TraceList
          rows={traces}
          selected={selected}
          onSelect={setSelected}
        />
        {trace ? (
          <TraceView
            trace={trace}
            diff={diff}
            cursor={cursor}
            setCursor={setCursor}
            playing={playing}
            setPlaying={setPlaying}
            flaggedIdxs={flaggedIdxs}
            names={names}
            labelFor={labelFor}
            renameCanister={renameCanister}
            hiddenCanisters={hiddenCanisters}
            toggleCanisterVisible={toggleCanisterVisible}
            collapsed={collapsed}
            toggleCollapse={toggleCollapse}
            exitForEnter={exitForEnter}
            hiddenByCollapse={hiddenByCollapse}
            visibleIndices={visibleIndices}
            searchMatches={searchMatches}
            search={search}
            setSearch={setSearch}
            searchOpen={searchOpen}
            setSearchOpen={setSearchOpen}
            showTimestamps={showTimestamps}
            setShowTimestamps={setShowTimestamps}
            stateMode={stateMode}
            setStateMode={setStateMode}
            traceStartNs={traceStartNs}
          />
        ) : (
          <div className="empty">
            {selected ? "loading…" : "no trace selected"}
          </div>
        )}
      </div>
    </div>
  );
}

// -------- Trace list --------
function TraceList({
  rows,
  selected,
  onSelect,
}: {
  rows: TraceRow[];
  selected: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <aside className="traces">
      <h2>traces</h2>
      {rows.length === 0 && <p className="muted">no traces yet</p>}
      <ul>
        {rows.map((t) => (
          <li
            key={t.trace_id}
            className={t.trace_id === selected ? "sel" : ""}
            onClick={() => onSelect(t.trace_id)}
          >
            <div className="tid">{t.trace_id.slice(0, 8)}…</div>
            <div className="meta">
              {t.event_count} events ·{" "}
              {new Date(t.started_at).toLocaleTimeString()}
            </div>
            {t.label && <div className="label">{t.label}</div>}
          </li>
        ))}
      </ul>
    </aside>
  );
}

// -------- Trace view (timeline + right panel) --------
function TraceView({
  trace, diff, cursor, setCursor, playing, setPlaying, flaggedIdxs,
  names, labelFor, renameCanister,
  hiddenCanisters, toggleCanisterVisible,
  collapsed, toggleCollapse, exitForEnter, hiddenByCollapse,
  visibleIndices, searchMatches, search, setSearch, searchOpen, setSearchOpen,
  showTimestamps, setShowTimestamps, stateMode, setStateMode, traceStartNs,
}: {
  trace: Trace; diff: DiffDoc | null; cursor: number;
  setCursor: (c: number) => void; playing: boolean;
  setPlaying: (p: boolean | ((p: boolean) => boolean)) => void;
  flaggedIdxs: number[]; names: NameMap; labelFor: (p: string) => string;
  renameCanister: (principal: string, name: string | null) => Promise<void>;
  hiddenCanisters: Set<string>; toggleCanisterVisible: (p: string) => void;
  collapsed: Set<number>; toggleCollapse: (i: number) => void;
  exitForEnter: Map<number, number>; hiddenByCollapse: Set<number>;
  visibleIndices: number[]; searchMatches: Set<number> | null;
  search: string; setSearch: (s: string) => void;
  searchOpen: boolean; setSearchOpen: (b: boolean) => void;
  showTimestamps: boolean; setShowTimestamps: (b: boolean) => void;
  stateMode: "history" | "current";
  setStateMode: (m: "history" | "current") => void;
  traceStartNs: number;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const flaggedSet = useMemo(() => new Set(flaggedIdxs), [flaggedIdxs]);

  const depthsByIdx = useMemo(() => {
    const per = new Map<string, number>();
    const out: number[] = [];
    for (const e of trace.events) {
      if (e.kind.kind === "method_exited") {
        const cur = per.get(e.canister) ?? 1;
        per.set(e.canister, Math.max(cur - 1, 0));
      }
      out.push(per.get(e.canister) ?? 0);
      if (e.kind.kind === "method_entered" || e.kind.kind === "ingress_entered")
        per.set(e.canister, (per.get(e.canister) ?? 0) + 1);
    }
    return out;
  }, [trace]);

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${cursor}"]`);
    if (el) el.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [cursor]);

  // Focus search input when it opens.
  useEffect(() => {
    if (searchOpen) searchRef.current?.focus();
  }, [searchOpen]);

  const current = trace.events[cursor];

  const tagWidth = useMemo(() => {
    const max = trace.summary.canisters
      .filter((c) => !hiddenCanisters.has(c))
      .reduce((m, c) => Math.max(m, labelFor(c).length), 0);
    return `calc(${max}ch + 10px)`;
  }, [trace.summary.canisters, labelFor, hiddenCanisters]);

  return (
    <main className="trace-main">
      <section className="center">
        <TraceHeader
          trace={trace} cursor={cursor} setCursor={setCursor}
          playing={playing} setPlaying={setPlaying}
          flagCount={flaggedIdxs.length} names={names} labelFor={labelFor}
          renameCanister={renameCanister}
          hiddenCanisters={hiddenCanisters}
          toggleCanisterVisible={toggleCanisterVisible}
          showTimestamps={showTimestamps} setShowTimestamps={setShowTimestamps}
          visibleIndices={visibleIndices}
        />
        {searchOpen && (
          <div className="search-bar">
            <span className="search-icon">⌕</span>
            <input
              ref={searchRef}
              className="search-input"
              placeholder="filter events…"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") { setSearchOpen(false); setSearch(""); }
              }}
            />
            {search && (
              <span className="search-count">
                {searchMatches?.size ?? 0} match{searchMatches?.size === 1 ? "" : "es"}
              </span>
            )}
            <button className="search-close" onClick={() => { setSearchOpen(false); setSearch(""); }}>✕</button>
          </div>
        )}
        <div className="timeline" ref={listRef}>
          {trace.events.map((e, i) => {
            if (hiddenCanisters.has(e.canister)) return null;
            if (hiddenByCollapse.has(i)) return null;

            const isEnter = e.kind.kind === "method_entered" || e.kind.kind === "ingress_entered";
            const isCollapsed = isEnter && collapsed.has(i);
            const exitIdx = exitForEnter.get(i);
            const hiddenCount = isCollapsed && exitIdx != null ? exitIdx - i : 0;

            const tag = labelFor(e.canister);
            const color = canisterColor(e.canister);
            const depth = depthsByIdx[i];
            const glyph = isCollapsed ? "▸" : kindGlyph(e.kind);
            const summary = kindSummary(e.kind, labelFor) +
              (isCollapsed ? `  ··· ${hiddenCount} hidden ···` : "");
            const isCursor = i === cursor;
            const isPast = i < cursor;
            const isErr = flaggedSet.has(i);
            const isDim = searchMatches != null && !searchMatches.has(i);
            const canCollapse = isEnter && exitIdx != null;

            const relMs = showTimestamps
              ? ((e.ts_nanos - traceStartNs) / 1e6).toFixed(1)
              : null;

            return (
              <div
                key={i}
                data-idx={i}
                className={[
                  "event",
                  isCursor ? "cursor" : "",
                  isPast ? "past" : "future",
                  isErr ? "flagged" : "",
                  isDim ? "dim" : "",
                ].join(" ")}
                onClick={() => setCursor(i)}
              >
                <span className="idx">#{String(e.idx).padStart(3, "0")}</span>
                <span className="tag" style={{ background: color, color: "#000", minWidth: tagWidth }}>
                  {tag}
                </span>
                <span className="flag-mark">{isErr ? "⚠" : ""}</span>
                {relMs !== null && <span className="ts">+{relMs}ms</span>}
                <span className="indent" style={{ paddingLeft: `${depth * 14}px` }}>
                  <span
                    className={`glyph ${canCollapse ? "collapsible" : ""}`}
                    onClick={canCollapse ? (ev) => { ev.stopPropagation(); toggleCollapse(i); } : undefined}
                    title={canCollapse ? (isCollapsed ? "expand" : "collapse") : undefined}
                  >{glyph}</span>
                  <span className="summary">{summary}</span>
                </span>
              </div>
            );
          })}
        </div>
      </section>
      <aside className="right">
        <EventDetail event={current} flagReason={current ? flagReason(current) : null} labelFor={labelFor} />
        <StatePanel
          diff={diff} trace={trace} cursor={cursor} labelFor={labelFor}
          stateMode={stateMode} setStateMode={setStateMode}
        />
      </aside>
    </main>
  );
}

// -------- Header + playback --------
function TraceHeader({
  trace, cursor, setCursor, playing, setPlaying, flagCount,
  names, labelFor, renameCanister,
  hiddenCanisters, toggleCanisterVisible,
  showTimestamps, setShowTimestamps, visibleIndices,
}: {
  trace: Trace; cursor: number; setCursor: (c: number) => void;
  playing: boolean; setPlaying: (p: boolean | ((p: boolean) => boolean)) => void;
  flagCount: number; names: NameMap; labelFor: (p: string) => string;
  renameCanister: (principal: string, name: string | null) => Promise<void>;
  hiddenCanisters: Set<string>; toggleCanisterVisible: (p: string) => void;
  showTimestamps: boolean; setShowTimestamps: (b: boolean) => void;
  visibleIndices: number[];
}) {
  const s = trace.summary;
  const n = visibleIndices.length;
  const visPos = visibleIndices.indexOf(cursor);

  const onRename = (principal: string) => {
    const existing = names[principal];
    const next = window.prompt(
      `Name for ${principal}\n(leave blank to reset to default)`,
      existing?.name ?? ""
    );
    if (next === null) return;
    void renameCanister(principal, next.trim() === "" ? null : next);
  };

  return (
    <div className="trace-header">
      <div className="trace-id">{s.trace_id}</div>
      <div className="trace-meta">
        {s.event_count} events · {s.canisters.length} canisters ·{" "}
        {s.call_spawned} calls · {s.rejects} rejects
        {flagCount > 0 && (
          <span className="error-pill">⚠ {flagCount} {flagCount === 1 ? "error" : "errors"}</span>
        )}
        <button
          className={`ts-toggle ${showTimestamps ? "active" : ""}`}
          title="toggle timestamps"
          onClick={() => setShowTimestamps(!showTimestamps)}
        >
          +ms
        </button>
      </div>
      <div className="legend">
        {s.canisters.map((c) => {
          const hidden = hiddenCanisters.has(c);
          const entry = names[c];
          const hint = entry
            ? `${c}\n${entry.name} (${entry.source})`
            : c;
          return (
            <span key={c} className={`legend-item-wrap ${hidden ? "hidden-can" : ""}`}>
              <span
                className="legend-item"
                style={{ background: canisterColor(c), color: "#000", opacity: hidden ? 0.35 : 1 }}
                title={hint}
              >
                {labelFor(c)}
              </span>
              <button
                className="legend-eye"
                title={hidden ? "show canister" : "hide canister"}
                onClick={() => toggleCanisterVisible(c)}
              >
                {hidden ? "○" : "◉"}
              </button>
              <button className="legend-rename" title="rename" onClick={() => onRename(c)}>✎</button>
            </span>
          );
        })}
      </div>
      <div className="playback">
        <button onClick={() => setCursor(visibleIndices[0] ?? 0)}>⏮</button>
        <button onClick={() => { const p = visibleIndices.findIndex(i => i >= cursor); setCursor(visibleIndices[Math.max(p - 1, 0)] ?? 0); }}>◀</button>
        <button onClick={() => setPlaying((p) => !p)}>{playing ? "⏸" : "▶"}</button>
        <button onClick={() => { const p = visibleIndices.indexOf(cursor); setCursor(visibleIndices[Math.min(p + 1, n - 1)] ?? 0); }}>▶</button>
        <button onClick={() => setCursor(visibleIndices[n - 1] ?? 0)}>⏭</button>
        <span className="cursor-pos">{visPos + 1} / {n}</span>
      </div>
    </div>
  );
}

// -------- Event detail --------
function EventDetail({
  event,
  flagReason,
  labelFor,
}: {
  event: EventRow | undefined;
  flagReason: string | null;
  labelFor: (p: string) => string;
}) {
  if (!event) return <div className="panel-section">no event</div>;
  const label = labelFor(event.canister);
  const showLabel = label !== shortPrincipal(event.canister);
  return (
    <div className="panel-section">
      {flagReason && <div className="flagged-banner">⚠ flagged: {flagReason}</div>}
      <h3>event #{event.idx}</h3>
      <div className="kv">
        <span>canister</span>
        <span style={{ color: canisterColor(event.canister) }}>
          {showLabel ? `${label} — ${event.canister}` : event.canister}
        </span>
      </div>
      <div className="kv">
        <span>seq</span>
        <span>{event.seq}</span>
      </div>
      <div className="kv">
        <span>parent_seq</span>
        <span>{event.parent_seq ?? "—"}</span>
      </div>
      <div className="kv">
        <span>span_id</span>
        <span>{event.span_id}</span>
      </div>
      <div className="kv">
        <span>kind</span>
        <span>{event.kind.kind}</span>
      </div>
      <pre className="raw">{JSON.stringify(event.kind, null, 2)}</pre>
    </div>
  );
}

// -------- State panel --------
function StatePanel({
  diff, trace, cursor, labelFor, stateMode, setStateMode,
}: {
  diff: DiffDoc | null; trace: Trace; cursor: number;
  labelFor: (p: string) => string;
  stateMode: "history" | "current";
  setStateMode: (m: "history" | "current") => void;
}) {
  if (!diff) return <div className="panel-section">loading diff…</div>;

  const events = trace.events;
  const seqByCanAtCursor = new Map<string, number>();
  for (let i = 0; i <= cursor; i++) seqByCanAtCursor.set(events[i].canister, events[i].seq);

  const firedTransitions = diff.transitions
    .filter((t) => { const s = seqByCanAtCursor.get(t.canister); return s != null && s >= t.to_seq; })
    .sort((a, b) => a.to_seq - b.to_seq);

  const firedInitials = diff.initials
    .filter((i) => { const s = seqByCanAtCursor.get(i.canister); return s != null && s >= i.seq; })
    .sort((a, b) => a.seq - b.seq);

  // "current" mode: one entry per (canister, key) — the latest known value.
  const displayTransitions = stateMode === "current"
    ? (() => {
        const latest = new Map<string, Transition>();
        for (const t of firedTransitions) latest.set(`${t.canister}:${t.key}`, t);
        return Array.from(latest.values()).sort((a, b) => a.to_seq - b.to_seq);
      })()
    : firedTransitions;
  const displayInitials = stateMode === "current"
    ? firedInitials.filter((i) => !firedTransitions.some((t) => t.canister === i.canister && t.key === i.key))
    : firedInitials;

  // Highlight whichever key the cursor event just wrote.
  const curEvent = events[cursor];
  const curKey = curEvent?.kind.kind === "state_snapshot" ? curEvent.kind.key : null;
  const curCan = curEvent?.canister;

  const isHighlighted = (canister: string, key: string) =>
    key === curKey && canister === curCan;

  return (
    <div className="panel-section state-panel">
      <div className="state-panel-header">
        <h3>state up to cursor</h3>
        <button
          className={`mode-toggle ${stateMode === "current" ? "active" : ""}`}
          onClick={() => setStateMode(stateMode === "current" ? "history" : "current")}
          title={stateMode === "current" ? "showing latest value per key — click for full history" : "showing full history — click for latest values only"}
        >
          {stateMode === "current" ? "current" : "history"}
        </button>
      </div>
      {displayTransitions.length === 0 && displayInitials.length === 0 && (
        <div className="muted">no state observed yet</div>
      )}
      {displayInitials.map((i) => (
        <div key={`i-${i.canister}-${i.key}-${i.seq}`} className={`state-entry ${isHighlighted(i.canister, i.key) ? "highlighted" : ""}`}>
          <div className="state-head">
            <span className="tag" style={{ background: canisterColor(i.canister), color: "#000" }} title={i.canister}>
              {labelFor(i.canister)}
            </span>
            <span className="key">{i.key}</span>
            <span className="muted small">@{i.seq}</span>
          </div>
          <pre className="state-value">{i.value}</pre>
        </div>
      ))}
      {displayTransitions.map((t, i) => (
        <div key={`t-${t.canister}-${t.key}-${t.to_seq}-${i}`} className={`state-entry ${isHighlighted(t.canister, t.key) ? "highlighted" : ""}`}>
          <div className="state-head">
            <span className="tag" style={{ background: canisterColor(t.canister), color: "#000" }} title={t.canister}>
              {labelFor(t.canister)}
            </span>
            <span className="key">{t.key}</span>
            <span className="muted small">
              {stateMode === "history" ? `${t.from_seq}→${t.to_seq}` : `@${t.to_seq}`}
            </span>
          </div>
          <div className="deltas">
            {t.deltas.map((d, j) => (
              <DeltaRow key={j} delta={d} />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

function DeltaRow({ delta }: { delta: Delta }) {
  if ("Added" in delta) {
    return (
      <div className="delta added">
        <span className="op">+</span>
        <span className="path">{delta.Added.path}</span>
        <span className="val">{delta.Added.value}</span>
      </div>
    );
  }
  if ("Removed" in delta) {
    return (
      <div className="delta removed">
        <span className="op">−</span>
        <span className="path">{delta.Removed.path}</span>
        <span className="val">{delta.Removed.value}</span>
      </div>
    );
  }
  const c = delta.Changed;
  return (
    <div className="delta changed">
      <span className="op">~</span>
      <span className="path">{c.path}</span>
      <span className="val">
        <span className="from">{c.from}</span>
        <span className="arrow"> → </span>
        <span className="to">{c.to}</span>
      </span>
    </div>
  );
}
