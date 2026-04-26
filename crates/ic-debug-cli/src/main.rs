use anyhow::Result;
use clap::{Parser, Subcommand};

mod diff;
mod instrument;
mod recorder;
mod replay;
mod serve;
mod store;

#[derive(Parser)]
#[command(name = "ic-debug", about = "Time-travel debugger for ICP canisters")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the recorder daemon (accepts events from agent + drains canisters).
    Record {
        #[arg(long, default_value = "traces/ic-debug.sqlite")]
        store: String,
        #[arg(long, default_value_t = 9191)]
        port: u16,
    },
    /// Replay a recorded trace — render the causal timeline step-by-step.
    Replay {
        /// Trace id (UUID) to replay.
        #[arg(long)]
        trace: String,
        /// SQLite store to read from.
        #[arg(long, default_value = "traces/ic-debug.sqlite")]
        store: String,
        /// Interactive: press enter after each event.
        #[arg(long)]
        step: bool,
        /// Decode StateSnapshot CBOR payloads into inline JSON-ish values.
        #[arg(long, default_value_t = true)]
        decode_cbor: bool,
        /// Emit the full trace as JSON (for programmatic consumers / UI).
        #[arg(long)]
        json: bool,
    },
    /// Diff state snapshots within a trace. Default: walk all (canister,
    /// key) pairs and show every consecutive transition. With
    /// --from/--to, diff a specific pair (requires --canister + --key).
    Diff {
        #[arg(long)]
        trace: String,
        #[arg(long, default_value = "traces/ic-debug.sqlite")]
        store: String,
        /// Scope to a single canister (principal text).
        #[arg(long)]
        canister: Option<String>,
        /// Scope to a single snapshot key (e.g. "payment").
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Walk Rust (`.rs`) or Motoko (`.mo`) source and interactively
    /// suggest tracing edits — `#[trace_method]` / `call_traced!` /
    /// `trace_event!` / `trace_state!` for Rust; the
    /// `tracer.beginTrace` / `methodEntered` / `methodExited`
    /// boilerplate plus `Trace.Tracer(...)` field and `__debug_drain`
    /// query for Motoko. Detection is purely syntactic and held to a
    /// zero-false-positive bar — see `docs/GUIDE.md` for the full
    /// list of rules and limits.
    Instrument {
        /// Path to a single .rs / .mo file, or a directory —
        /// directories are walked recursively (skipping `target/`,
        /// `node_modules/`, hidden dirs, build/dist) and every .rs
        /// and .mo file is processed.
        path: std::path::PathBuf,
        /// Print the candidate list and exit without prompting or
        /// writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip prompts: emit the unified diff for every candidate to
        /// stdout. Useful for scripted review; no file is modified.
        #[arg(long)]
        diff_only: bool,
        /// Accept every candidate without prompting and write the file.
        /// Rule 4 (snapshot a constructed local) uses the binding name
        /// as the snapshot key. Use this only for scripted instrumentation
        /// where you trust the rule set; otherwise prefer the interactive
        /// flow which lets you see the source context for each suggestion.
        #[arg(long)]
        apply_all: bool,
        /// Root directory for the agent-js post-step scan (only runs if
        /// Rule 1b accepts a TraceHeader insertion). The post-step
        /// looks for `<method>: IDL.Func([...])` and `<actor>.<method>(...)`
        /// patterns and offers to splice in `Header,` / `trace.header(),`.
        #[arg(long, default_value = "agent-js")]
        agent_js_root: std::path::PathBuf,
        /// Skip the agent-js post-step entirely. Default behaviour is
        /// to run the scan only when the agent-js root exists *and* a
        /// Rule 1b candidate was accepted.
        #[arg(long)]
        skip_agent_js: bool,
    },
    /// Serve the web UI + JSON API over the trace store.
    Serve {
        #[arg(long, default_value = "traces/ic-debug.sqlite")]
        store: String,
        #[arg(long, default_value_t = 9192)]
        port: u16,
        /// Directory of the built UI (Vite dist/). Falls back to index.html
        /// for unknown paths so client-side routing works.
        #[arg(long, default_value = "ui/dist")]
        ui_dir: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ic_debug=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Record { store, port } => recorder::run(&store, port).await,
        Cmd::Replay { trace, store, step, decode_cbor, json } => {
            replay::run(replay::Options {
                store,
                trace,
                step,
                decode_cbor,
                json,
            })
        }
        Cmd::Diff { trace, store, canister, key, from, to, json } => {
            diff::run(diff::Options {
                store,
                trace,
                canister,
                key,
                from,
                to,
                json,
            })
        }
        Cmd::Serve { store, port, ui_dir } => serve::run(&store, port, &ui_dir).await,
        Cmd::Instrument {
            path,
            dry_run,
            diff_only,
            apply_all,
            agent_js_root,
            skip_agent_js,
        } => instrument::run(instrument::Options {
            path,
            dry_run,
            diff_only,
            apply_all,
            agent_js_root,
            skip_agent_js,
        }),
    }
}
