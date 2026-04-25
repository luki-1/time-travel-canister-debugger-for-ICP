//! Six detection rules. Each one is held to the zero-false-positive
//! contract from the plan: the rule fires only when its preconditions
//! prove the suggested edit is syntactically valid Rust whose runtime
//! behaviour matches the intent. When in doubt the rule does not fire —
//! the user adds the missed site by hand, same as today.
//!
//! The rules:
//!   1. WrapMethod     — add `#[trace_method]` above an `#[update]` etc.
//!   2. ConvertCall    — rewrite `ic_cdk::call` to `call_traced!`.
//!   3. EntryNote      — `trace_event!("<fn>:enter")` as first body stmt.
//!   4. SnapshotLocal  — `trace_state!` after `let x = SomeStruct {…};`.
//!   5. RollbackNote   — `trace_event!("<fn>:rollback")` before a literal
//!                       `return Err(…)` when state was traced earlier in
//!                       the same block.
//!   6. TrapNote       — `trace_event!("<fn>:trapped")` before an
//!                       `ic_cdk::trap` call.

use std::collections::HashSet;

use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::{
    Attribute, Block, Expr, ExprCall, ExprMethodCall, ExprReturn, File, FnArg, ItemFn, Macro,
    Pat, PatIdent, ReturnType, Stmt,
};

use super::parse::{path_matches, type_ends_with, AliasMap};
use super::{Candidate, Replacement, Rule};

const CDK_ENTRY_ATTRS: &[&str] = &["update", "query", "heartbeat", "inspect_message"];
const CDK_ENTRY_PATHS: &[&str] = &[
    "ic_cdk::update",
    "ic_cdk::query",
    "ic_cdk::heartbeat",
    "ic_cdk::inspect_message",
    "ic_cdk_macros::update",
    "ic_cdk_macros::query",
    "ic_cdk_macros::heartbeat",
    "ic_cdk_macros::inspect_message",
];

const TRACE_METHOD_PATHS: &[&str] = &[
    "trace_method",
    "ic_debug_trace_macros::trace_method",
    "ic_debug_trace::trace_method",
];

const IC_CDK_CALL_TARGETS: &[&str] = &["ic_cdk::call"];
const IC_CDK_TRAP_TARGETS: &[&str] = &["ic_cdk::trap", "ic_cdk::api::trap"];

pub fn all(file: &File, src: &str, aliases: &AliasMap) -> Vec<Candidate> {
    let containers = collect_thread_local_idents(file);
    let mut out = Vec::new();
    for item in &file.items {
        if let syn::Item::Fn(f) = item {
            scan_fn(f, src, aliases, &containers, &mut out);
        }
    }
    out.sort_by_key(|c| c.byte_range.start);
    out
}

/// Walk top-level items and collect every ident declared inside a
/// `thread_local! { static <NAME>: … = …; }` block. Rule 7 only fires
/// on `<NAME>.with(…)` calls where `<NAME>` is in this set, so a
/// `Foo.with(...)` for some unrelated type cannot trigger a false
/// positive.
fn collect_thread_local_idents(file: &File) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &file.items {
        let syn::Item::Macro(m) = item else { continue };
        if !macro_path_ident_is(&m.mac.path, "thread_local") {
            continue;
        }
        // Walk the token stream looking for `static <IDENT>:` patterns.
        // proc-macro-level scanning rather than re-parsing keeps this
        // dep-light and tolerant of evolving thread_local! syntax.
        let toks: Vec<_> = m.mac.tokens.clone().into_iter().collect();
        for w in toks.windows(3) {
            let (a, b, c) = (&w[0], &w[1], &w[2]);
            if let (
                proc_macro2::TokenTree::Ident(kw),
                proc_macro2::TokenTree::Ident(name),
                proc_macro2::TokenTree::Punct(p),
            ) = (a, b, c)
            {
                if kw == "static" && p.as_char() == ':' {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

fn scan_fn(
    f: &ItemFn,
    src: &str,
    aliases: &AliasMap,
    containers: &HashSet<String>,
    out: &mut Vec<Candidate>,
) {
    let has_trace_method = f.attrs.iter().any(attr_is_trace_method);
    let cdk_attr = f.attrs.iter().find(|a| attr_is_cdk_entry(a));

    // Rule 1 fires when a CDK entry method has TraceHeader as first arg
    // but no `#[trace_method]` yet.
    let rule_1_applies =
        !has_trace_method && cdk_attr.is_some() && first_arg_is_trace_header(f);
    if rule_1_applies {
        out.push(make_wrap_method_candidate(f, cdk_attr.unwrap(), src));
    }

    // Rule 1b fires when a CDK *write* entry method has no TraceHeader
    // and no `#[trace_method]`. We deliberately skip `#[query]`: read
    // methods rarely benefit from the breaking signature change, and
    // utility queries like `__debug_drain` exist precisely because
    // they're outside the trace boundary.
    let rule_1b_applies = !has_trace_method
        && cdk_attr.map(attr_is_cdk_write_entry).unwrap_or(false)
        && !first_arg_is_trace_header(f);
    if rule_1b_applies {
        out.push(make_wrap_method_insert_header_candidate(
            f,
            cdk_attr.unwrap(),
            src,
        ));
    }

    // Body-level rules require a traced context. We treat "already has
    // `#[trace_method]`" and "Rule 1 / 1b just suggested adding it" the
    // same way, so accepting either doesn't cost the user a second
    // wizard pass to discover body candidates. Rule 1b is excluded from
    // this cascade because the body still references the missing
    // `header` parameter — Rules 4/5/etc. could fire fine, but the
    // user is likely going to refactor the body after the breaking
    // change anyway, so we keep the candidate set small and let them
    // re-run the wizard once the dust settles.
    if !has_trace_method && !rule_1_applies {
        return;
    }

    let fn_name = f.sig.ident.to_string();
    let returns_result = sig_returns_result(&f.sig);

    // Rule 3 — entry note.
    scan_entry_note(f, &fn_name, src, out);

    // Body-level rules — Rule 2, 4, 5, 6, 7.
    scan_block(
        &f.block,
        &Ctx {
            fn_name: &fn_name,
            returns_result,
            aliases,
            containers,
            src,
        },
        out,
    );
}

struct Ctx<'a> {
    fn_name: &'a str,
    returns_result: bool,
    aliases: &'a AliasMap,
    containers: &'a HashSet<String>,
    src: &'a str,
}

// ---------- Rule 1 ----------

// ---------- Rule 1b ----------

fn make_wrap_method_insert_header_candidate(
    f: &ItemFn,
    cdk: &Attribute,
    src: &str,
) -> Candidate {
    let attr_start = byte_offset(src, cdk.span(), Anchor::Start);
    let line_start = line_start_offset(src, attr_start);
    let indent = src[line_start..attr_start].to_string();

    // Two atomic edits:
    //   1. Insert `#[trace_method]\n<indent>` at attr_start.
    //   2. Insert `header: TraceHeader, ` (or `header: TraceHeader`
    //      with no trailing comma) into the parameter list.
    let attr_edit = (
        attr_start..attr_start,
        format!("#[trace_method]\n{indent}"),
    );

    // Find the byte offset of the `(` that opens the parameter list.
    // We search forward from the fn's ident; the fn signature spans
    // through the body, so we slice and find the first `(`.
    let sig_start = byte_offset(src, f.sig.ident.span(), Anchor::End);
    let paren_open = src[sig_start..]
        .find('(')
        .map(|i| sig_start + i)
        .unwrap_or(sig_start);
    let insert_pos = paren_open + 1;

    let param_edit = if f.sig.inputs.is_empty() {
        (insert_pos..insert_pos, "header: TraceHeader".to_string())
    } else {
        (insert_pos..insert_pos, "header: TraceHeader, ".to_string())
    };

    Candidate {
        rule: Rule::WrapMethodInsertHeader,
        byte_range: attr_start..attr_start,
        fn_name: f.sig.ident.to_string(),
        summary: format!(
            "wrap `{}` with #[trace_method] AND insert `header: TraceHeader` first param",
            f.sig.ident
        ),
        warning: Some(
            "BREAKING API CHANGE: every caller of this method (including \
             agent-js scripts and other canisters) must be updated to pass \
             a TraceHeader as the first argument."
                .to_string(),
        ),
        replacement: Replacement::Multi(vec![attr_edit, param_edit]),
    }
}

// ---------- Rule 1 ----------

fn make_wrap_method_candidate(f: &ItemFn, cdk: &Attribute, src: &str) -> Candidate {
    let attr_start = byte_offset(src, cdk.span(), Anchor::Start);
    let line_start = line_start_offset(src, attr_start);
    let indent = &src[line_start..attr_start];
    // Insert at attr_start (after the line's indent) so the new line
    // takes its indentation from the *trailing* part — and the existing
    // attr line keeps the indent it already had.
    let replacement = format!("#[trace_method]\n{indent}");
    Candidate {
        rule: Rule::WrapMethod,
        byte_range: attr_start..attr_start,
        fn_name: f.sig.ident.to_string(),
        summary: format!("wrap `{}` with #[trace_method]", f.sig.ident),
        warning: None,
        replacement: Replacement::InsertRaw(replacement),
    }
}

// ---------- Rule 3 ----------

fn scan_entry_note(f: &ItemFn, fn_name: &str, src: &str, out: &mut Vec<Candidate>) {
    let body = &f.block;
    if first_n_stmts_contain_entry_note(body, 3) {
        return;
    }
    let Some(first_stmt) = body.stmts.first() else {
        // Empty body — nothing to instrument meaningfully.
        return;
    };
    let stmt_start = byte_offset(src, first_stmt.span(), Anchor::Start);
    let line_start = line_start_offset(src, stmt_start);
    let indent = &src[line_start..stmt_start];
    let inserted = format!(
        "trace_event!(\"{fn_name}:enter\");\n{indent}",
        fn_name = fn_name
    );
    out.push(Candidate {
        rule: Rule::EntryNote,
        byte_range: stmt_start..stmt_start,
        fn_name: fn_name.to_string(),
        summary: format!("emit trace_event!(\"{fn_name}:enter\") at top of body"),
        warning: None,
        replacement: Replacement::InsertRaw(inserted),
    });
}

/// True if any of the first `n` statements is a `trace_event!(...)` whose
/// label string ends with ":enter". We don't require an exact match on
/// the function name — users often prefix the label with a module name
/// (e.g. `"escrow.lock_funds:enter"`) and we should respect their
/// convention rather than insert a second entry note alongside it.
fn first_n_stmts_contain_entry_note(body: &Block, n: usize) -> bool {
    body.stmts.iter().take(n).any(stmt_is_trace_event_enter)
}

fn stmt_is_trace_event_enter(stmt: &Stmt) -> bool {
    let Some(mac) = stmt_as_macro(stmt) else { return false };
    if !macro_path_ident_is(&mac.path, "trace_event") {
        return false;
    }
    macro_first_arg_string_ends_with(&mac.tokens, ":enter")
}

// ---------- Body walker ----------

fn scan_block(b: &Block, ctx: &Ctx<'_>, out: &mut Vec<Candidate>) {
    let mut state_traced_in_block = false;

    for (idx, stmt) in b.stmts.iter().enumerate() {
        // Rule 5 precondition: track whether a trace_state! has appeared.
        if stmt_is_trace_state(stmt) {
            state_traced_in_block = true;
        }

        // Rule 4 — snapshot a constructed local (`let x = Foo { … };`).
        if let Stmt::Local(local) = stmt {
            if let Some(c) = rule_snapshot_local(local, &b.stmts[idx + 1..], ctx) {
                out.push(c);
            }
        }

        // Rule 5 — rollback note before `return Err(...)` (literal).
        if ctx.returns_result && state_traced_in_block {
            if let Some(c) = rule_rollback_note(stmt, ctx, b, idx) {
                out.push(c);
            }
        }

        // Rule 7 — mutation snapshot after `<NAME>.with(|x| … borrow_mut() …)`.
        if let Some(c) = rule_mutation_snapshot(stmt, &b.stmts[idx + 1..], ctx) {
            out.push(c);
        }

        // Walk the expression for Rules 2 (call) and 6 (trap), and
        // recurse into nested blocks (if/match/while/for/loop bodies).
        scan_stmt_exprs(stmt, ctx, out);
    }
}

fn scan_stmt_exprs(stmt: &Stmt, ctx: &Ctx<'_>, out: &mut Vec<Candidate>) {
    match stmt {
        Stmt::Local(l) => {
            if let Some(init) = &l.init {
                scan_expr(&init.expr, ctx, out);
            }
        }
        Stmt::Expr(e, _) => scan_expr(e, ctx, out),
        Stmt::Macro(_) => {} // skip — we don't recurse through macro bodies
        Stmt::Item(_) => {}  // skip nested item defs
    }
}

fn scan_expr(e: &Expr, ctx: &Ctx<'_>, out: &mut Vec<Candidate>) {
    // Rules 2 + 6: direct call expressions.
    if let Expr::Call(call) = e {
        if let Some(c) = rule_convert_call(call, ctx) {
            out.push(c);
        }
        if let Some(c) = rule_trap_note(call, ctx) {
            out.push(c);
        }
    }

    // Recurse into structural sub-expressions. We deliberately skip
    // closures and async blocks: emitting a `trace_event!` inside a
    // closure body would fire at unexpected times. For zero false
    // positives we simply don't look there.
    match e {
        Expr::Block(b) => scan_block(&b.block, ctx, out),
        Expr::If(i) => {
            scan_expr(&i.cond, ctx, out);
            scan_block(&i.then_branch, ctx, out);
            if let Some((_, else_branch)) = &i.else_branch {
                scan_expr(else_branch, ctx, out);
            }
        }
        Expr::Match(m) => {
            scan_expr(&m.expr, ctx, out);
            for arm in &m.arms {
                if let Some((_, guard)) = &arm.guard {
                    scan_expr(guard, ctx, out);
                }
                scan_expr(&arm.body, ctx, out);
            }
        }
        Expr::While(w) => {
            scan_expr(&w.cond, ctx, out);
            scan_block(&w.body, ctx, out);
        }
        Expr::ForLoop(f) => {
            scan_expr(&f.expr, ctx, out);
            scan_block(&f.body, ctx, out);
        }
        Expr::Loop(l) => scan_block(&l.body, ctx, out),
        Expr::Unsafe(u) => scan_block(&u.block, ctx, out),
        Expr::Return(r) => {
            if let Some(v) = &r.expr {
                scan_expr(v, ctx, out);
            }
        }
        Expr::Try(t) => scan_expr(&t.expr, ctx, out),
        Expr::Await(a) => scan_expr(&a.base, ctx, out),
        Expr::MethodCall(m) => {
            scan_expr(&m.receiver, ctx, out);
            for arg in &m.args {
                scan_expr(arg, ctx, out);
            }
        }
        Expr::Call(c) => {
            scan_expr(&c.func, ctx, out);
            for arg in &c.args {
                scan_expr(arg, ctx, out);
            }
        }
        Expr::Binary(b) => {
            scan_expr(&b.left, ctx, out);
            scan_expr(&b.right, ctx, out);
        }
        Expr::Unary(u) => scan_expr(&u.expr, ctx, out),
        Expr::Let(l) => scan_expr(&l.expr, ctx, out),
        Expr::Reference(r) => scan_expr(&r.expr, ctx, out),
        Expr::Tuple(t) => {
            for el in &t.elems {
                scan_expr(el, ctx, out);
            }
        }
        Expr::Array(a) => {
            for el in &a.elems {
                scan_expr(el, ctx, out);
            }
        }
        Expr::Paren(p) => scan_expr(&p.expr, ctx, out),
        // Expr::Closure intentionally skipped.
        // Expr::Async intentionally skipped.
        _ => {}
    }
}

// ---------- Rule 2 ----------

fn rule_convert_call(call: &ExprCall, ctx: &Ctx<'_>) -> Option<Candidate> {
    let path = match &*call.func {
        Expr::Path(p) if p.qself.is_none() => &p.path,
        _ => return None,
    };
    let _canonical = path_matches(path, ctx.aliases, IC_CDK_CALL_TARGETS)?;
    if call.args.len() != 3 {
        return None;
    }

    let span_start = byte_offset(ctx.src, call.span(), Anchor::Start);
    let span_end = byte_offset(ctx.src, call.span(), Anchor::End);

    // Build replacement: `call_traced!(<arg1>, <arg2>, <arg3>)`. The
    // tuple of args (third arg) is already a tuple expression in the
    // source; we re-emit it verbatim by slicing the src bytes.
    let arg_slices: Vec<&str> = call
        .args
        .iter()
        .map(|a| &ctx.src[byte_offset(ctx.src, a.span(), Anchor::Start)..byte_offset(ctx.src, a.span(), Anchor::End)])
        .collect();
    let replacement = format!(
        "call_traced!({}, {}, {})  /* TODO: confirm return type */",
        arg_slices[0], arg_slices[1], arg_slices[2]
    );

    Some(Candidate {
        rule: Rule::ConvertCall,
        byte_range: span_start..span_end,
        fn_name: ctx.fn_name.to_string(),
        summary: "convert ic_cdk::call to call_traced!".to_string(),
        warning: Some(
            "the callee canister MUST accept TraceHeader as its first argument"
                .to_string(),
        ),
        replacement: Replacement::Replace(replacement),
    })
}

// ---------- Rule 4 ----------

fn rule_snapshot_local(
    local: &syn::Local,
    rest: &[Stmt],
    ctx: &Ctx<'_>,
) -> Option<Candidate> {
    let ident = match &local.pat {
        Pat::Ident(PatIdent { ident, .. }) => ident.to_string(),
        Pat::Type(pt) => match &*pt.pat {
            Pat::Ident(PatIdent { ident, .. }) => ident.to_string(),
            _ => return None,
        },
        _ => return None,
    };
    let init = local.init.as_ref()?;
    if !matches!(&*init.expr, Expr::Struct(_)) {
        return None;
    }

    // Idempotency: skip if any of the next 10 statements is a
    // `trace_state!(…)` whose token stream mentions this binding as a
    // bare ident. We walk the token tree (rather than `to_string()`
    // matching) to ignore appearances of the same name inside string
    // literals — the key arg of `trace_state!` is itself a string and
    // could match by accident.
    for s in rest.iter().take(10) {
        if let Some(m) = stmt_as_macro(s) {
            if macro_path_ident_is(&m.path, "trace_state")
                && tokens_mention_ident(&m.tokens, &ident)
            {
                return None;
            }
        }
    }

    // Insert just after the `;` ending the `let` — leading newline so we
    // start a fresh line, no trailing newline because the original `\n`
    // after the `;` is still there.
    let local_end = byte_offset(ctx.src, local.span(), Anchor::End);
    let stmt_start = byte_offset(ctx.src, local.span(), Anchor::Start);
    let stmt_line_start = line_start_offset(ctx.src, stmt_start);
    let indent = ctx.src[stmt_line_start..stmt_start].to_string();
    let template = format!("\n{indent}trace_state!(\"{{KEY}}\", &{ident});");

    Some(Candidate {
        rule: Rule::SnapshotLocal,
        byte_range: local_end..local_end,
        fn_name: ctx.fn_name.to_string(),
        summary: format!("snapshot `{ident}` after construction"),
        warning: None,
        replacement: Replacement::InsertAfterWithKey {
            template,
            default_key: ident,
        },
    })
}

// ---------- Rule 5 ----------

fn rule_rollback_note(stmt: &Stmt, ctx: &Ctx<'_>, _block: &Block, _idx: usize) -> Option<Candidate> {
    // Match a literal `return Err(…);` statement.
    let ret = match stmt {
        Stmt::Expr(Expr::Return(r), _) => r,
        _ => return None,
    };
    if !return_is_err(ret) {
        return None;
    }

    // Idempotency: don't fire if the previous stmt is already
    // trace_event!("<fn>:rollback").
    // We can't peek backward easily here; do it via byte scan instead.
    let stmt_start = byte_offset(ctx.src, stmt.span(), Anchor::Start);
    let line_start = line_start_offset(ctx.src, stmt_start);
    let preceding = preceding_non_empty_line(ctx.src, line_start);
    if line_has_trace_event_with_suffix(preceding, ":rollback") {
        return None;
    }

    let indent = ctx.src[line_start..stmt_start].to_string();
    let inserted = format!(
        "{indent}trace_event!(\"{fn_name}:rollback\");\n",
        indent = indent,
        fn_name = ctx.fn_name
    );
    Some(Candidate {
        rule: Rule::RollbackNote,
        byte_range: line_start..line_start,
        fn_name: ctx.fn_name.to_string(),
        summary: format!(
            "emit trace_event!(\"{fn}:rollback\") before this return",
            fn = ctx.fn_name
        ),
        warning: None,
        replacement: Replacement::InsertRaw(inserted),
    })
}

fn return_is_err(ret: &ExprReturn) -> bool {
    let Some(v) = &ret.expr else { return false };
    if let Expr::Call(c) = &**v {
        if let Expr::Path(p) = &*c.func {
            if let Some(seg) = p.path.segments.last() {
                return seg.ident == "Err";
            }
        }
    }
    false
}

// ---------- Rule 7 ----------

fn rule_mutation_snapshot(
    stmt: &Stmt,
    rest: &[Stmt],
    ctx: &Ctx<'_>,
) -> Option<Candidate> {
    // We require the *statement* to be the with-call, so we don't fire
    // on things like `let foo = LOCKS.with(...)` (which would be the
    // user already capturing a value — they probably want to handle
    // the snapshot themselves around that binding).
    let expr = match stmt {
        Stmt::Expr(e, Some(_)) => e,
        _ => return None,
    };
    let mc = peel_to_method_call(expr, "with")?;
    let receiver = receiver_ident(&mc.receiver)?;
    if !ctx.containers.contains(&receiver) {
        return None;
    }
    if mc.args.len() != 1 {
        return None;
    }
    let closure = match &mc.args[0] {
        Expr::Closure(c) => c,
        _ => return None,
    };
    if !expr_mentions_method(&closure.body, "borrow_mut") {
        return None;
    }

    // Idempotency: the user already snapshotted if either
    //   (a) the closure body itself emits `trace_state!`, OR
    //   (b) any of the next 3 statements is a `trace_state!` —
    //       regardless of what value it references, because the user
    //       knows their own keying convention better than we do
    //       (e.g. `LOCKS.with(...)` followed by `trace_state!("lock",
    //       &lock)` is the canonical pattern).
    if expr_mentions_macro(&closure.body, "trace_state") {
        return None;
    }
    for s in rest.iter().take(3) {
        if let Some(m) = stmt_as_macro(s) {
            if macro_path_ident_is(&m.path, "trace_state") {
                return None;
            }
        }
    }

    let stmt_end = byte_offset(ctx.src, stmt.span(), Anchor::End);
    let stmt_start = byte_offset(ctx.src, stmt.span(), Anchor::Start);
    let line_start = line_start_offset(ctx.src, stmt_start);
    let indent = ctx.src[line_start..stmt_start].to_string();
    let default_key = receiver.to_lowercase();
    let value_hint = format!("&{default_key}");
    let template = format!("\n{indent}trace_state!(\"{{KEY}}\", {{VALUE}});");

    Some(Candidate {
        rule: Rule::MutationSnapshot,
        byte_range: stmt_end..stmt_end,
        fn_name: ctx.fn_name.to_string(),
        summary: format!(
            "snapshot state after `{receiver}.with(...)` mutation"
        ),
        warning: Some(
            "you'll be asked for the snapshot key and value expression. \
             Pick a value that captures what changed (e.g. `&new_lock`) — \
             the wizard cannot infer it from the closure body."
                .to_string(),
        ),
        replacement: Replacement::InsertWithKeyAndValue {
            template,
            default_key,
            value_hint,
        },
    })
}

/// If `e` is a method call (possibly behind a `.await` or a paren
/// wrapper), return the `ExprMethodCall` if its method ident matches
/// `name`. We deliberately don't peel through any other transforms —
/// e.g. `if cond { LOCKS.with(...) }` should NOT trigger Rule 7
/// because the snapshot site after the `if` is ambiguous.
fn peel_to_method_call<'a>(e: &'a Expr, name: &str) -> Option<&'a ExprMethodCall> {
    match e {
        Expr::MethodCall(m) if m.method == name => Some(m),
        Expr::Await(a) => peel_to_method_call(&a.base, name),
        Expr::Paren(p) => peel_to_method_call(&p.expr, name),
        _ => None,
    }
}

fn receiver_ident(e: &Expr) -> Option<String> {
    if let Expr::Path(p) = e {
        if p.qself.is_none() && p.path.segments.len() == 1 {
            return Some(p.path.segments[0].ident.to_string());
        }
    }
    None
}

/// True if `e` (or any sub-expression we walk through) contains a
/// `.<method>(...)` call. Used by Rule 7 to confirm the closure body
/// actually does a `borrow_mut()` rather than a read-only `.borrow()`.
fn expr_mentions_method(e: &Expr, method: &str) -> bool {
    match e {
        Expr::MethodCall(m) => {
            if m.method == method {
                return true;
            }
            if expr_mentions_method(&m.receiver, method) {
                return true;
            }
            m.args.iter().any(|a| expr_mentions_method(a, method))
        }
        Expr::Block(b) => block_mentions_method(&b.block, method),
        Expr::Closure(c) => expr_mentions_method(&c.body, method),
        Expr::If(i) => {
            expr_mentions_method(&i.cond, method)
                || block_mentions_method(&i.then_branch, method)
                || i.else_branch
                    .as_ref()
                    .map(|(_, e)| expr_mentions_method(e, method))
                    .unwrap_or(false)
        }
        Expr::Match(m) => {
            expr_mentions_method(&m.expr, method)
                || m.arms.iter().any(|a| expr_mentions_method(&a.body, method))
        }
        Expr::While(w) => {
            expr_mentions_method(&w.cond, method)
                || block_mentions_method(&w.body, method)
        }
        Expr::ForLoop(f) => {
            expr_mentions_method(&f.expr, method) || block_mentions_method(&f.body, method)
        }
        Expr::Loop(l) => block_mentions_method(&l.body, method),
        Expr::Call(c) => {
            expr_mentions_method(&c.func, method)
                || c.args.iter().any(|a| expr_mentions_method(a, method))
        }
        Expr::Binary(b) => {
            expr_mentions_method(&b.left, method) || expr_mentions_method(&b.right, method)
        }
        Expr::Unary(u) => expr_mentions_method(&u.expr, method),
        Expr::Reference(r) => expr_mentions_method(&r.expr, method),
        Expr::Paren(p) => expr_mentions_method(&p.expr, method),
        Expr::Tuple(t) => t.elems.iter().any(|x| expr_mentions_method(x, method)),
        Expr::Return(r) => r.expr.as_ref().map(|e| expr_mentions_method(e, method)).unwrap_or(false),
        Expr::Try(t) => expr_mentions_method(&t.expr, method),
        Expr::Await(a) => expr_mentions_method(&a.base, method),
        Expr::Let(l) => expr_mentions_method(&l.expr, method),
        Expr::Assign(a) => {
            expr_mentions_method(&a.left, method) || expr_mentions_method(&a.right, method)
        }
        _ => false,
    }
}

/// True if `e` (or anything we walk through) contains a macro call
/// whose path's last segment is `name`. Used by Rule 7 to detect
/// `trace_state!` invocations inside the with-closure's body.
fn expr_mentions_macro(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Macro(em) => macro_path_ident_is(&em.mac.path, name),
        Expr::Block(b) => block_mentions_macro(&b.block, name),
        Expr::Closure(c) => expr_mentions_macro(&c.body, name),
        Expr::If(i) => {
            expr_mentions_macro(&i.cond, name)
                || block_mentions_macro(&i.then_branch, name)
                || i.else_branch
                    .as_ref()
                    .map(|(_, e)| expr_mentions_macro(e, name))
                    .unwrap_or(false)
        }
        Expr::Match(m) => {
            expr_mentions_macro(&m.expr, name)
                || m.arms.iter().any(|a| expr_mentions_macro(&a.body, name))
        }
        Expr::While(w) => {
            expr_mentions_macro(&w.cond, name) || block_mentions_macro(&w.body, name)
        }
        Expr::ForLoop(f) => {
            expr_mentions_macro(&f.expr, name) || block_mentions_macro(&f.body, name)
        }
        Expr::Loop(l) => block_mentions_macro(&l.body, name),
        Expr::MethodCall(m) => {
            expr_mentions_macro(&m.receiver, name)
                || m.args.iter().any(|a| expr_mentions_macro(a, name))
        }
        Expr::Call(c) => {
            expr_mentions_macro(&c.func, name)
                || c.args.iter().any(|a| expr_mentions_macro(a, name))
        }
        Expr::Reference(r) => expr_mentions_macro(&r.expr, name),
        Expr::Paren(p) => expr_mentions_macro(&p.expr, name),
        Expr::Await(a) => expr_mentions_macro(&a.base, name),
        Expr::Try(t) => expr_mentions_macro(&t.expr, name),
        Expr::Return(r) => r
            .expr
            .as_ref()
            .map(|e| expr_mentions_macro(e, name))
            .unwrap_or(false),
        _ => false,
    }
}

fn block_mentions_macro(b: &Block, name: &str) -> bool {
    for s in &b.stmts {
        let m = match s {
            Stmt::Macro(sm) => macro_path_ident_is(&sm.mac.path, name),
            Stmt::Expr(e, _) => expr_mentions_macro(e, name),
            Stmt::Local(l) => l
                .init
                .as_ref()
                .map(|i| expr_mentions_macro(&i.expr, name))
                .unwrap_or(false),
            _ => false,
        };
        if m {
            return true;
        }
    }
    false
}

fn block_mentions_method(b: &Block, method: &str) -> bool {
    for s in &b.stmts {
        let mentions = match s {
            Stmt::Local(l) => l
                .init
                .as_ref()
                .map(|i| expr_mentions_method(&i.expr, method))
                .unwrap_or(false),
            Stmt::Expr(e, _) => expr_mentions_method(e, method),
            _ => false,
        };
        if mentions {
            return true;
        }
    }
    false
}

// ---------- Rule 6 ----------

fn rule_trap_note(call: &ExprCall, ctx: &Ctx<'_>) -> Option<Candidate> {
    let path = match &*call.func {
        Expr::Path(p) if p.qself.is_none() => &p.path,
        _ => return None,
    };
    let _ = path_matches(path, ctx.aliases, IC_CDK_TRAP_TARGETS)?;

    let span_start = byte_offset(ctx.src, call.span(), Anchor::Start);
    let line_start = line_start_offset(ctx.src, span_start);
    let preceding = preceding_non_empty_line(ctx.src, line_start);
    if line_has_trace_event_with_suffix(preceding, ":trapped") {
        return None;
    }
    let indent = ctx.src[line_start..span_start].to_string();
    let inserted = format!(
        "{indent}trace_event!(\"{fn_name}:trapped\");\n",
        indent = indent,
        fn_name = ctx.fn_name
    );
    Some(Candidate {
        rule: Rule::TrapNote,
        byte_range: line_start..line_start,
        fn_name: ctx.fn_name.to_string(),
        summary: format!(
            "emit trace_event!(\"{fn}:trapped\") before this trap",
            fn = ctx.fn_name
        ),
        warning: None,
        replacement: Replacement::InsertRaw(inserted),
    })
}

// ---------- helpers ----------

fn attr_is_trace_method(a: &Attribute) -> bool {
    let p = a.path();
    let joined: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
    let full = joined.join("::");
    TRACE_METHOD_PATHS.iter().any(|t| *t == full)
}

fn attr_is_cdk_entry(a: &Attribute) -> bool {
    let p = a.path();
    if p.segments.len() == 1 {
        let n = p.segments[0].ident.to_string();
        return CDK_ENTRY_ATTRS.iter().any(|t| *t == n);
    }
    let joined: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
    let full = joined.join("::");
    CDK_ENTRY_PATHS.iter().any(|t| *t == full)
}

/// Subset of CDK entry attrs that imply a *write* method — i.e. ones
/// where adding a TraceHeader makes sense. `#[query]` is excluded;
/// queries are read-only and rarely worth the breaking change.
fn attr_is_cdk_write_entry(a: &Attribute) -> bool {
    let p = a.path();
    let last = p.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
    matches!(
        last.as_str(),
        "update" | "heartbeat" | "inspect_message" | "init" | "post_upgrade"
    )
}

fn first_arg_is_trace_header(f: &ItemFn) -> bool {
    let arg = f.sig.inputs.first();
    let Some(FnArg::Typed(pt)) = arg else { return false };
    type_ends_with(&pt.ty, "TraceHeader")
}

fn sig_returns_result(sig: &syn::Signature) -> bool {
    match &sig.output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => type_ends_with(ty, "Result"),
    }
}

fn stmt_is_trace_state(s: &Stmt) -> bool {
    let Some(m) = stmt_as_macro(s) else { return false };
    macro_path_ident_is(&m.path, "trace_state")
}

/// Returns the inner `Macro` for both `Stmt::Macro` (a macro call as a
/// statement, e.g. `trace_event!("…");`) and `Stmt::Expr(Expr::Macro)`
/// (the same call as a trailing tail expression).
fn stmt_as_macro(s: &Stmt) -> Option<&Macro> {
    match s {
        Stmt::Macro(sm) => Some(&sm.mac),
        Stmt::Expr(Expr::Macro(em), _) => Some(&em.mac),
        _ => None,
    }
}

fn macro_path_ident_is(p: &syn::Path, ident: &str) -> bool {
    p.segments
        .last()
        .map(|s| s.ident == ident)
        .unwrap_or(false)
}

fn tokens_mention_ident(tokens: &proc_macro2::TokenStream, ident: &str) -> bool {
    use proc_macro2::TokenTree;
    for tt in tokens.clone() {
        match tt {
            TokenTree::Ident(i) if i == ident => return true,
            TokenTree::Group(g) => {
                if tokens_mention_ident(&g.stream(), ident) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn macro_first_arg_string_ends_with(tokens: &proc_macro2::TokenStream, suffix: &str) -> bool {
    use proc_macro2::TokenTree;
    for tt in tokens.clone() {
        if let TokenTree::Literal(lit) = tt {
            let s = lit.to_string();
            if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
                return s[1..s.len() - 1].ends_with(suffix);
            }
        }
    }
    false
}

#[derive(Copy, Clone)]
enum Anchor {
    Start,
    End,
}

fn byte_offset(src: &str, span: Span, which: Anchor) -> usize {
    let lc = match which {
        Anchor::Start => span.start(),
        Anchor::End => span.end(),
    };
    line_col_to_byte(src, lc.line, lc.column)
}

fn line_col_to_byte(src: &str, line: usize, col: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let mut cur_line = 1;
    let mut byte = 0usize;
    for (i, ch) in src.char_indices() {
        if cur_line == line {
            // Walk `col` chars (utf-8 chars, matching how proc_macro2
            // measures column positions).
            let mut taken = 0usize;
            let mut j = i;
            for (off, _ch) in src[i..].char_indices() {
                if taken == col {
                    return i + off;
                }
                if &src[i + off..i + off + 1] == "\n" {
                    return i + off;
                }
                taken += 1;
                j = i + off;
            }
            return j + 1;
        }
        if ch == '\n' {
            cur_line += 1;
            byte = i + 1;
        }
    }
    byte
}

fn line_start_offset(src: &str, offset: usize) -> usize {
    let bound = offset.min(src.len());
    src[..bound].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// True if `line` looks like `…trace_event!("X")…` where X ends with
/// `suffix`. We accept any prefix on X so user conventions like
/// `escrow.lock_funds:rollback` are recognised.
fn line_has_trace_event_with_suffix(line: &str, suffix: &str) -> bool {
    let Some(start) = line.find("trace_event!(") else { return false };
    let after = &line[start + "trace_event!(".len()..];
    let Some(open) = after.find('"') else { return false };
    let lit_start = open + 1;
    let Some(close_off) = after[lit_start..].find('"') else { return false };
    let lit = &after[lit_start..lit_start + close_off];
    lit.ends_with(suffix)
}

fn preceding_non_empty_line(src: &str, line_start: usize) -> &str {
    if line_start == 0 {
        return "";
    }
    let prev = &src[..line_start - 1];
    let prev_start = prev.rfind('\n').map(|i| i + 1).unwrap_or(0);
    &src[prev_start..line_start - 1]
}
