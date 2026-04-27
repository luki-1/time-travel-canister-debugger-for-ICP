//! `ic-debug instrument` — interactive tracing setup wizard.
//!
//! Walks one Rust source file, finds candidate spots for ic-debug
//! instrumentation, prompts the user, and writes the resulting edits.
//! Detection is purely syntactic; every rule is documented in `detect.rs`
//! and held to a zero-false-positives bar — if a rule cannot prove the
//! edit is correct, it does not fire.

use anyhow::{Context, Result};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

mod agent_js;
mod detect;
mod diff;
mod edit;
mod motoko;
mod parse;
mod prompt;

#[cfg(test)]
mod tests;

pub struct Options {
    pub path: PathBuf,
    pub dry_run: bool,
    pub diff_only: bool,
    pub apply_all: bool,
    /// Root for the agent-js scan triggered after Rule 1b accepts. If
    /// the directory doesn't exist, the post-step is silently skipped.
    pub agent_js_root: PathBuf,
    /// Skip the agent-js post-step entirely.
    pub skip_agent_js: bool,
}

/// One detection. The byte range is the slice of source the edit affects;
/// `replacement` says how to combine the new text with that range.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub rule: Rule,
    pub byte_range: Range<usize>,
    pub fn_name: String,
    /// One-line summary shown in the prompt header, e.g.
    /// "wrap `lock_funds` with `#[trace_method]`".
    pub summary: String,
    /// Optional warning line shown beneath the summary (Rule 2 uses this).
    pub warning: Option<String>,
    pub replacement: Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    WrapMethod,
    /// Like WrapMethod, but the function lacks a `TraceHeader` first
    /// argument — the wizard inserts both the parameter and the
    /// attribute. This is a *breaking change* for callers and is
    /// presented with an explicit warning.
    WrapMethodInsertHeader,
    ConvertCall,
    EntryNote,
    SnapshotLocal,
    /// State container detection: a `<NAME>.with(|x| … x.borrow_mut() …)`
    /// pattern in a traced method body, where `<NAME>` is the ident of
    /// a `thread_local!` declaration in the same file. The wizard
    /// suggests a `trace_state!` snapshot after the call; the user
    /// supplies key + value expression at prompt time.
    MutationSnapshot,
    RollbackNote,
    TrapNote,
    // ---- Motoko rules ----
    /// Insert `import Trace`, the `tracer` field, and `__debug_drain`
    /// query in an actor that has none of them yet.
    MoBootstrap,
    /// Insert `tracer.beginTrace(header) / methodEntered / methodExited`
    /// boilerplate in a public method that already takes
    /// `header : ?<alias>.TraceHeader` as its first parameter.
    MoWrapMethod,
    /// Like MoWrapMethod, but the method has no `?TraceHeader` first
    /// parameter — the wizard inserts both the parameter and the body
    /// boilerplate. Breaking ABI change; warned about at the prompt.
    MoWrapMethodInsertHeader,
    /// Insert `tracer.note("<fn>:enter")` after `methodEntered` when
    /// no entry note is present near the top of the body.
    MoEntryNote,
    /// Insert `tracer.note("<fn>:trapped")` before a `Debug.trap(...)`
    /// call inside a traced method body.
    MoTrapNote,
    /// Insert `tracer.note("<fn>:rollback")` before a `throw` expression
    /// inside a traced method body. Mirrors Rust's rollback-note rule.
    MoRollbackNote,
    /// Insert `tracer.snapshotText("<key>", debug_show <var>)` after an
    /// assignment to an actor-level `var` inside a traced method body.
    /// Only fires on bare-ident LHS (not record field or array element
    /// updates) and only when the var is declared at actor scope, not
    /// as a local shadow. Mirrors Rust's mutation-snapshot rule.
    MoMutationSnapshot,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::WrapMethod => "wrap-method",
            Rule::WrapMethodInsertHeader => "wrap-method-insert-header",
            Rule::ConvertCall => "convert-call",
            Rule::EntryNote => "entry-note",
            Rule::SnapshotLocal => "snapshot-local",
            Rule::MutationSnapshot => "mutation-snapshot",
            Rule::RollbackNote => "rollback-note",
            Rule::TrapNote => "trap-note",
            Rule::MoBootstrap => "mo-bootstrap",
            Rule::MoWrapMethod => "mo-wrap-method",
            Rule::MoWrapMethodInsertHeader => "mo-wrap-method-insert-header",
            Rule::MoEntryNote => "mo-entry-note",
            Rule::MoTrapNote => "mo-trap-note",
            Rule::MoRollbackNote => "mo-rollback-note",
            Rule::MoMutationSnapshot => "mo-mutation-snapshot",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Replacement {
    /// Replace `byte_range` with the supplied text verbatim.
    Replace(String),
    /// Insert text at `byte_range.start` (zero-width range) verbatim —
    /// the rule that built it is responsible for any indentation and
    /// trailing newline.
    InsertRaw(String),
    /// Like `InsertRaw`, but the text contains a `{KEY}` placeholder
    /// the prompt loop substitutes with the user's input (Rule 4).
    InsertAfterWithKey { template: String, default_key: String },
    /// Apply several `(byte_range, text)` edits atomically as one
    /// candidate — used by Rule 1b which inserts both an attribute and
    /// a parameter, and by Rule 7 (mutation snapshot) which can need
    /// both a key and a value.
    Multi(Vec<(Range<usize>, String)>),
    /// Two-key variant for Rule 7 (mutation snapshot): the template
    /// contains `{KEY}` and `{VALUE}` placeholders the prompt loop
    /// substitutes with user input. `default_key` is offered as the
    /// snapshot key default; `value_hint` is shown to the user as the
    /// suggested value expression but has no useful default.
    InsertWithKeyAndValue {
        template: String,
        default_key: String,
        value_hint: String,
    },
}

pub fn run(opts: Options) -> Result<()> {
    if opts.path.is_dir() {
        return run_directory(&opts);
    }
    run_one_file(&opts.path, &opts)
}

/// Walk a directory recursively and run the wizard on every supported
/// source file (`.rs` + `.mo`), skipping the usual build/VCS noise.
/// Each file is handled independently — a `--dry-run` aggregates
/// findings across files; an `--apply-all` rewrites them in place; the
/// interactive flow prompts per file in source order.
fn run_directory(opts: &Options) -> Result<()> {
    let files = collect_source_files(&opts.path)?;
    if files.is_empty() {
        println!("no .rs or .mo files under {}", opts.path.display());
        return Ok(());
    }
    println!("instrumenting {} file(s) under {}", files.len(), opts.path.display());
    for f in files {
        if let Err(e) = run_one_file(&f, opts) {
            eprintln!("error processing {}: {e:#}", f.display());
        }
    }
    Ok(())
}

fn collect_source_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // Skip directories that never contain hand-written canister
            // source. Hidden dirs are skipped by name; build artefacts
            // and vendored deps are listed explicitly.
            if name.starts_with('.')
                || matches!(name.as_ref(), "target" | "node_modules" | "dist" | "build")
            {
                continue;
            }
            walk(&path, out)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs") | Some("mo")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

fn run_one_file(path: &Path, opts: &Options) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str());
    let src = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let candidates = match ext {
        Some("rs") => {
            let file = syn::parse_file(&src)
                .with_context(|| format!("parse {} as Rust", path.display()))?;
            let aliases = parse::collect_aliases(&file);
            detect::all(&file, &src, &aliases)
        }
        Some("mo") => motoko::detect(&src, path),
        _ => Vec::new(),
    };

    if candidates.is_empty() {
        if !opts.diff_only {
            println!("no instrumentation candidates in {}", path.display());
        }
        return Ok(());
    }

    if opts.dry_run {
        println!("\n── {} ──", path.display());
        print_dry_run(&candidates, &src);
        return Ok(());
    }

    let accepted = if opts.diff_only || opts.apply_all {
        // Both paths accept everything detection produced. `--diff-only`
        // emits the diff and exits; `--apply-all` writes the file
        // without further prompting.
        candidates
    } else {
        prompt::run_loop(&candidates, &src)?
    };

    if accepted.is_empty() {
        println!("nothing accepted; file unchanged.");
        return Ok(());
    }

    let new_src = edit::apply(&src, &accepted)?;
    let diff_text = diff::unified(&src, &new_src, path);

    if opts.diff_only {
        print!("{diff_text}");
        return Ok(());
    }

    if !opts.apply_all {
        println!("\n{diff_text}");
        if !prompt::confirm_apply()? {
            println!("aborted; file unchanged.");
            return Ok(());
        }
    }

    fs::write(path, &new_src)
        .with_context(|| format!("write {}", path.display()))?;
    println!("wrote {} ({} edits applied)", path.display(), accepted.len());

    // Post-step: for every Rule 1b candidate that landed, scan the
    // agent-js root for callers and offer to update them. The Rule 1b
    // edit changed the canister's public ABI; the caller-side updates
    // restore compatibility.
    if !opts.skip_agent_js {
        run_agent_js_post(&accepted, opts)?;
    }
    Ok(())
}

fn run_agent_js_post(accepted: &[Candidate], opts: &Options) -> Result<()> {
    // Both the Rust and Motoko "insert TraceHeader" rules change the
    // canister ABI; the agent-js scan is the same for either.
    let methods: Vec<&str> = accepted
        .iter()
        .filter(|c| {
            matches!(
                c.rule,
                Rule::WrapMethodInsertHeader | Rule::MoWrapMethodInsertHeader
            )
        })
        .map(|c| c.fn_name.as_str())
        .collect();
    if methods.is_empty() {
        return Ok(());
    }
    if !opts.agent_js_root.exists() {
        return Ok(()); // nothing to scan
    }
    println!(
        "\n── agent-js post-step ── scanning {} for {} method(s)",
        opts.agent_js_root.display(),
        methods.len()
    );
    let mut all_refs = Vec::new();
    for m in &methods {
        let refs = agent_js::find_references(m, &opts.agent_js_root)?;
        if !refs.is_empty() {
            println!(
                "  `{}` referenced in {} place(s):",
                m,
                refs.len()
            );
            for r in &refs {
                println!(
                    "    {} {}:{}  {}",
                    r.kind.as_str(),
                    r.path.display(),
                    r.line_no,
                    r.line_text.trim()
                );
            }
        }
        all_refs.extend(refs);
    }
    if all_refs.is_empty() {
        println!("  no agent-js references found.");
        return Ok(());
    }

    // For both --apply-all and --diff-only we apply automatically; the
    // interactive flow asks once before writing.
    if !opts.apply_all && !opts.diff_only {
        if !prompt::confirm_apply_agent_js(all_refs.len())? {
            println!("  agent-js updates skipped.");
            return Ok(());
        }
    }
    let edits = agent_js::apply(&all_refs)?;
    println!("  applied {edits} agent-js edit(s).");
    Ok(())
}

fn print_dry_run(candidates: &[Candidate], src: &str) {
    println!("found {} candidates:\n", candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        let line = line_of_offset(src, c.byte_range.start);
        println!(
            "  [{}] {} (line {}, fn `{}`) — {}",
            i + 1,
            c.rule.as_str(),
            line,
            c.fn_name,
            c.summary
        );
        if let Some(w) = &c.warning {
            println!("       ⚠ {w}");
        }
    }
}

fn line_of_offset(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count() + 1
}
