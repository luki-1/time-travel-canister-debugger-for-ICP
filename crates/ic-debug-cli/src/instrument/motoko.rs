//! Motoko instrumentation rules — sister to `detect.rs` for Rust.
//!
//! Five rules, each held to the same zero-false-positive bar as the
//! Rust pipeline. The Motoko Trace API is library calls (no macros),
//! so each rule's edit is several lines of explicit
//! `tracer.<method>(...)` invocations rather than a single attribute.
//!
//! The rules:
//!   1. mo-bootstrap                   — actor lacks the `tracer` field
//!                                       entirely. Insert
//!                                       `import Trace`, the field, and
//!                                       `__debug_drain` in one go.
//!   2. mo-wrap-method                 — public func with
//!                                       `header : ?<alias>.TraceHeader`
//!                                       first param but no
//!                                       `tracer.beginTrace` in body.
//!   3. mo-wrap-method-insert-header   — public write func with no
//!                                       header param. Breaking change.
//!   4. mo-entry-note                  — `methodEntered` present, no
//!                                       matching `tracer.note(":enter")`.
//!   5. mo-trap-note                   — `Debug.trap(...)` inside a
//!                                       traced body, no preceding
//!                                       `tracer.note(":trapped")`.
//!
//! Parsing strategy: Motoko has no Rust-grade parser available, so we
//! do a careful character scan that tracks string/comment state and
//! finds balanced `{...}` blocks. Detection patterns then operate on
//! line-level substrings within those blocks. False positives are
//! avoided by requiring multiple corroborating signals (e.g. the
//! tracer-field detector must see *both* `Trace.Tracer(` and a `let`
//! introducing `tracer`).

use std::ops::Range;
use std::path::Path;

use super::{Candidate, Replacement, Rule};

// ---------- Public entry ----------

pub fn detect(src: &str, path: &Path) -> Vec<Candidate> {
    let Some(info) = parse(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    if let Some(c) = rule_bootstrap(&info, src, path) {
        out.push(c);
    }
    for f in &info.funcs {
        if let Some(c) = rule_wrap_method(f, &info, src) {
            out.push(c);
        }
        if let Some(c) = rule_wrap_method_insert_header(f, &info, src) {
            out.push(c);
        }
        if let Some(c) = rule_entry_note(f, &info, src) {
            out.push(c);
        }
        for trap in &f.trap_calls {
            if let Some(c) = rule_trap_note(f, trap, src) {
                out.push(c);
            }
        }
    }

    out.sort_by_key(|c| c.byte_range.start);
    out
}

// ---------- Parser model ----------

struct ActorInfo {
    /// Byte offset right after the actor's opening `{` (suitable for
    /// inserting new declarations as the first body item).
    insert_after_open: usize,
    /// Byte offset of the actor's closing `}` (suitable for inserting
    /// new declarations as the last body item).
    insert_before_close: usize,
    /// Alias of the Trace module — derived from imports, defaulting
    /// to "Trace" if no `Trace`-shaped import is present.
    trace_alias: String,
    /// Range of the line the trace import sits on, or None if missing.
    /// Used to detect bootstrap idempotency.
    trace_import_line: Option<Range<usize>>,
    /// Set if any `let <ident> = <alias>.Tracer(` was found in the
    /// actor body — bootstrap is skipped when this is present.
    has_tracer_field: bool,
    /// Identifier the user bound the `Tracer(...)` call to (e.g.
    /// `tracer`). Defaults to "tracer" if unset.
    tracer_ident: String,
    /// Set if `__debug_drain` is defined anywhere in the body.
    has_drain: bool,
    /// All public funcs found in the actor body, in source order.
    funcs: Vec<PubFunc>,
}

struct Import {
    /// Identifier the module is bound to, e.g. `Trace`.
    alias: String,
    /// Quoted path string, e.g. `../../../../motoko/src/Trace`.
    path: String,
}

struct PubFunc {
    name: String,
    /// Body interior, between the opening `{` and closing `}`.
    body_range: Range<usize>,
    /// Position of the opening `{` of the body.
    body_open: usize,
    /// Position right after the opening `(` of the parameter list.
    params_open: usize,
    /// Whether the parameter list is empty (only whitespace between
    /// the parens).
    params_empty: bool,
    /// Whether the first parameter is `header : ?<alias>.TraceHeader`
    /// (or `header : ?TraceHeader` when no alias was used).
    header_first: bool,
    /// Whether the function is declared `public query func`. Queries
    /// are excluded from the insert-header rule because adding the
    /// header to a read method is rarely worth the breaking change.
    is_query: bool,
    /// Whether the function is declared `public shared` (so we can
    /// reference `msg.caller` in inserted boilerplate). When false we
    /// fall back to `Principal.fromActor(self)` as the caller.
    is_shared: bool,
    /// Whether the body contains `<tracer>.beginTrace(`.
    has_begin_trace: bool,
    /// Whether the body contains `<tracer>.methodEntered(`.
    has_method_entered: bool,
    /// Whether the body has a `tracer.note("…:enter")` substring
    /// somewhere in the first few lines.
    has_entry_note: bool,
    /// `Debug.trap(...)` calls found inside this function's body.
    trap_calls: Vec<TrapCall>,
}

struct TrapCall {
    /// Byte offset of the `D` in `Debug.trap`.
    start: usize,
}

fn parse(src: &str) -> Option<ActorInfo> {
    let imports = parse_imports(src);
    let actor_body = find_actor_body(src)?;

    let body_str = &src[actor_body.clone()];
    let body_offset = actor_body.start;

    let trace_import_line = imports
        .iter()
        .find(|i| import_is_trace(&i.path))
        .and_then(|imp| find_import_line(src, imp));
    let trace_alias = imports
        .iter()
        .find(|i| import_is_trace(&i.path))
        .map(|i| i.alias.clone())
        .unwrap_or_else(|| "Trace".to_string());

    let (has_tracer_field, tracer_ident) = find_tracer_field(body_str, &trace_alias);
    let has_drain = body_str.contains("func __debug_drain");
    let funcs = find_public_funcs(body_str, body_offset, &trace_alias, &tracer_ident);

    Some(ActorInfo {
        insert_after_open: actor_body.start,
        insert_before_close: actor_body.end,
        trace_alias,
        trace_import_line,
        has_tracer_field,
        tracer_ident,
        has_drain,
        funcs,
    })
}

// ---------- Tokeniser-aware scanner ----------

/// Walk `src` producing a vector flagging which bytes are "code"
/// (outside strings and comments) vs "skipped". Used by all the
/// structural finders below so we never see braces or keywords inside
/// `"hello { world"` or `// {`.
fn skip_mask(src: &str) -> Vec<bool> {
    let bytes = src.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        // Line comment: // … \n
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            for m in mask.iter_mut().take(i).skip(start) {
                *m = false;
            }
            continue;
        }
        // Block comment: /* … */ (Motoko allows nesting).
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            for m in mask.iter_mut().take(i).skip(start) {
                *m = false;
            }
            continue;
        }
        // String literal: "…" with \\ escapes.
        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1; // consume closing quote
            }
            for m in mask.iter_mut().take(i).skip(start) {
                *m = false;
            }
            continue;
        }
        i += 1;
    }
    mask
}

/// Find the matching `}` for the `{` at byte position `open` in `src`,
/// honoring `mask` so braces inside strings/comments are skipped.
fn match_brace(src: &str, mask: &[bool], open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if open >= bytes.len() || bytes[open] != b'{' {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        if mask[i] {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Scan forward from `start` for the next byte-position where `pat` is
/// a substring AND every byte of the match is in code (mask=true).
fn find_in_code(src: &str, mask: &[bool], start: usize, pat: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let pat_bytes = pat.as_bytes();
    if pat_bytes.is_empty() || pat_bytes.len() > bytes.len() {
        return None;
    }
    let mut i = start;
    while i + pat_bytes.len() <= bytes.len() {
        if &bytes[i..i + pat_bytes.len()] == pat_bytes
            && mask[i..i + pat_bytes.len()].iter().all(|b| *b)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ---------- Imports ----------

fn parse_imports(src: &str) -> Vec<Import> {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("import ") {
            continue;
        }
        // import <Alias> "<path>" ; — quotes are required by Motoko.
        let rest = &trimmed[7..];
        let alias_end = rest
            .find(|c: char| !is_ident_char(c))
            .unwrap_or(rest.len());
        let alias = rest[..alias_end].to_string();
        if alias.is_empty() {
            continue;
        }
        let after_alias = &rest[alias_end..];
        let Some(q1) = after_alias.find('"') else {
            continue;
        };
        let after_q1 = &after_alias[q1 + 1..];
        let Some(q2) = after_q1.find('"') else {
            continue;
        };
        let path = after_q1[..q2].to_string();
        out.push(Import { alias, path });
    }
    out
}

fn import_is_trace(path: &str) -> bool {
    // Last path component of an import like `…/Trace` or `…/Trace.mo`.
    let last = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let last = last.strip_suffix(".mo").unwrap_or(last);
    last == "Trace"
}

fn find_import_line(src: &str, imp: &Import) -> Option<Range<usize>> {
    // Locate the line whose contents match the import we parsed. We
    // search for both the alias and the path so we don't get fooled by
    // unrelated comment text mentioning "Trace".
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("import ")
            && trimmed.contains(&imp.alias)
            && line.contains(&imp.path)
        {
            return Some(offset..offset + line.len());
        }
        offset += line.len();
    }
    None
}

// ---------- Actor body ----------

fn find_actor_body(src: &str) -> Option<Range<usize>> {
    let mask = skip_mask(src);
    // Look for `actor` followed by either `self` / `<name>` / nothing
    // and then `{`. Allow `persistent` / `shared` / leading whitespace
    // before `actor`.
    let actor_kw = find_in_code(src, &mask, 0, "actor")?;
    // Scan forward for the first `{` in code after the keyword.
    let bytes = src.as_bytes();
    let mut i = actor_kw + "actor".len();
    while i < bytes.len() {
        if mask[i] && bytes[i] == b'{' {
            let close = match_brace(src, &mask, i)?;
            // Body interior: byte after `{`, byte at `}`.
            return Some(i + 1..close);
        }
        i += 1;
    }
    None
}

// ---------- Tracer field ----------

fn find_tracer_field(body: &str, trace_alias: &str) -> (bool, String) {
    let mask = skip_mask(body);
    let pat = format!("{trace_alias}.Tracer(");
    let Some(call_at) = find_in_code(body, &mask, 0, &pat) else {
        return (false, "tracer".to_string());
    };
    // Walk backwards from `call_at` to find the binding ident: …
    // `let <ident> = <alias>.Tracer(...)` or
    // `transient let <ident> = …`.
    let prefix = &body[..call_at];
    // Find the `=` immediately before the call.
    let Some(eq) = prefix.rfind('=') else {
        return (true, "tracer".to_string());
    };
    let before_eq = &prefix[..eq].trim_end();
    // Take the last whitespace-separated token as the ident.
    let ident = before_eq
        .rsplit_once(|c: char| c.is_whitespace())
        .map(|(_, tail)| tail.trim().to_string())
        .unwrap_or_else(|| "tracer".to_string());
    let ident = if ident.chars().all(is_ident_char) && !ident.is_empty() {
        ident
    } else {
        "tracer".to_string()
    };
    (true, ident)
}

// ---------- Public funcs ----------

fn find_public_funcs(
    body: &str,
    body_offset: usize,
    trace_alias: &str,
    tracer_ident: &str,
) -> Vec<PubFunc> {
    let mask = skip_mask(body);
    let mut out = Vec::new();
    let mut i = 0;
    let bytes = body.as_bytes();

    // We anchor on the keyword `func` (in code) and walk back to verify
    // the modifier prefix begins with `public`. This handles all four
    // shapes: `public func`, `public shared func`, `public shared (msg)
    // func`, `public query func`.
    while i + 4 <= bytes.len() {
        if !mask[i] || &bytes[i..i + 4] != b"func" {
            i += 1;
            continue;
        }
        // Bound check: ensure word boundary on either side.
        if i > 0 && is_ident_char(bytes[i - 1] as char) {
            i += 1;
            continue;
        }
        if i + 4 < bytes.len() && is_ident_char(bytes[i + 4] as char) {
            i += 1;
            continue;
        }

        // Walk back through whitespace + modifiers (`shared (msg)`,
        // `query`) to confirm the keyword chain starts with `public`.
        let prefix_end = i;
        let mut prefix_start = i;
        while prefix_start > 0 {
            let ch = bytes[prefix_start - 1] as char;
            if ch.is_whitespace() || ch == ')' || ch == '(' || ch == ',' || is_ident_char(ch) {
                prefix_start -= 1;
            } else {
                break;
            }
        }
        let prefix = &body[prefix_start..prefix_end];
        let is_public = prefix.split_whitespace().any(|t| t == "public");
        let is_query = prefix.split_whitespace().any(|t| t == "query");
        let is_shared = prefix.split_whitespace().any(|t| t == "shared");
        if !is_public {
            i = prefix_end + 4;
            continue;
        }

        // Parse the rest: `func <name>(<params>) : <ret> { <body> }`.
        let mut j = i + 4;
        skip_ws_and_comments(body, &mask, &mut j);
        // Identifier.
        let name_start = j;
        while j < bytes.len() && is_ident_char(bytes[j] as char) {
            j += 1;
        }
        let name = body[name_start..j].to_string();
        if name.is_empty() {
            i = i + 4;
            continue;
        }
        skip_ws_and_comments(body, &mask, &mut j);
        // Opening paren.
        if j >= bytes.len() || bytes[j] != b'(' {
            i = i + 4;
            continue;
        }
        let params_open = j + 1;
        // Find the matching ')'.
        let Some(params_close) = match_paren(body, &mask, j) else {
            i = i + 4;
            continue;
        };
        // Body opening brace — scan forward in code.
        let mut k = params_close + 1;
        while k < bytes.len() && (!mask[k] || bytes[k] != b'{') {
            k += 1;
        }
        if k >= bytes.len() {
            i = i + 4;
            continue;
        }
        let body_open = k;
        let Some(body_close) = match_brace(body, &mask, body_open) else {
            i = i + 4;
            continue;
        };

        // End of statement: the `;` or newline after the closing brace.
        let mut full_end = body_close + 1;
        while full_end < bytes.len() && (bytes[full_end] == b';' || bytes[full_end] == b' ') {
            full_end += 1;
        }

        // Param introspection.
        let params_str = &body[params_open..params_close];
        let params_empty = params_str.trim().is_empty();
        let header_first = first_param_is_trace_header(params_str, trace_alias);

        // Body introspection.
        let body_str = &body[body_open + 1..body_close];
        let body_mask = skip_mask(body_str);
        let begin_pat = format!("{tracer_ident}.beginTrace(");
        let entered_pat = format!("{tracer_ident}.methodEntered(");
        let has_begin_trace = find_in_code(body_str, &body_mask, 0, &begin_pat).is_some();
        let has_method_entered =
            find_in_code(body_str, &body_mask, 0, &entered_pat).is_some();
        let has_entry_note = body_has_entry_note(body_str, &body_mask, tracer_ident);

        let trap_calls = find_trap_calls(body_str, &body_mask, body_offset + body_open + 1);

        out.push(PubFunc {
            name,
            body_range: (body_offset + body_open + 1)..(body_offset + body_close),
            body_open: body_offset + body_open,
            params_open: body_offset + params_open,
            params_empty,
            header_first,
            is_query,
            is_shared,
            has_begin_trace,
            has_method_entered,
            has_entry_note,
            trap_calls,
        });

        i = full_end;
    }

    out
}

fn skip_ws_and_comments(src: &str, mask: &[bool], i: &mut usize) {
    let bytes = src.as_bytes();
    while *i < bytes.len()
        && (!mask[*i] || (bytes[*i] as char).is_whitespace())
    {
        *i += 1;
    }
}

fn match_paren(src: &str, mask: &[bool], open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if open >= bytes.len() || bytes[open] != b'(' {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        if mask[i] {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn first_param_is_trace_header(params: &str, alias: &str) -> bool {
    let trimmed = params.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    // Take just the first parameter (split on top-level `,` — at this
    // depth no nested parens / brackets are likely in a TraceHeader
    // declaration, but we still respect string/comment skipping).
    let mask = skip_mask(params);
    let bytes = params.as_bytes();
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_brack = 0i32;
    let mut end = bytes.len();
    for (idx, &b) in bytes.iter().enumerate() {
        if !mask[idx] {
            continue;
        }
        match b {
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b'[' => depth_brack += 1,
            b']' => depth_brack -= 1,
            b',' if depth_paren == 0 && depth_brace == 0 && depth_brack == 0 => {
                end = idx;
                break;
            }
            _ => {}
        }
    }
    let first = params[..end].trim();
    // Match either `?<alias>.TraceHeader` or `?TraceHeader` or
    // `?Trace.TraceHeader` literally.
    let header_t = "TraceHeader";
    let alias_form = format!("?{alias}.{header_t}");
    let bare_form = format!("?{header_t}");
    let plain_form = format!("?Trace.{header_t}");
    first.contains(&alias_form) || first.contains(&bare_form) || first.contains(&plain_form)
}

fn body_has_entry_note(body: &str, mask: &[bool], tracer_ident: &str) -> bool {
    // We only require `<tracer>.note("…:enter"…)` somewhere in the
    // first ~5 statements (proxy: first 10 lines). The exact label is
    // not constrained — users prefix with module names like
    // "escrow.lock_funds:enter".
    let pat = format!("{tracer_ident}.note(");
    let mut search_from = 0;
    let lines_we_care_about = body
        .lines()
        .take(10)
        .map(|l| l.len() + 1)
        .sum::<usize>()
        .min(body.len());
    while let Some(at) = find_in_code(body, mask, search_from, &pat) {
        if at >= lines_we_care_about {
            return false;
        }
        // Look for the next `"…:enter"` literal on the same call.
        let after = &body[at + pat.len()..];
        let Some(q1) = after.find('"') else { return false };
        let after_q1 = &after[q1 + 1..];
        let Some(q2) = after_q1.find('"') else { return false };
        let lit = &after_q1[..q2];
        if lit.ends_with(":enter") {
            return true;
        }
        search_from = at + pat.len();
    }
    false
}

fn find_trap_calls(body: &str, mask: &[bool], offset: usize) -> Vec<TrapCall> {
    let mut out = Vec::new();
    let mut i = 0;
    let pat = "Debug.trap";
    while let Some(at) = find_in_code(body, mask, i, pat) {
        // Word boundary: next char must be `(` (after optional space).
        let after = &body[at + pat.len()..];
        let after_trim = after.trim_start();
        if after_trim.starts_with('(') {
            out.push(TrapCall { start: offset + at });
        }
        i = at + pat.len();
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Last byte offset of a `;` that's in code (not inside a string or
/// comment). Used to find the insertion point for `methodExited` when
/// the body ends with a trailing return expression.
fn last_semicolon_in_code(body: &str, mask: &[bool]) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if mask[i] && bytes[i] == b';' {
            return Some(i);
        }
    }
    None
}

// ---------- Rules ----------

fn rule_bootstrap(info: &ActorInfo, src: &str, path: &Path) -> Option<Candidate> {
    if info.has_tracer_field {
        return None;
    }
    if info.funcs.is_empty() {
        // Nothing to instrument anyway — skip to avoid noise.
        return None;
    }

    let actor_indent = leading_indent_of_line(src, info.insert_after_open);
    let inner_indent = format!("{actor_indent}  ");

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut summary_parts: Vec<&str> = Vec::new();

    if info.trace_import_line.is_none() {
        // Insert `import Trace "<resolved path>";` after the last
        // existing import, or at the top of the file if there are
        // none.
        let trace_path = guess_trace_import_path(path);
        let new_line = format!("import Trace \"{trace_path}\";\n");
        let pos = last_import_end(src).unwrap_or(0);
        edits.push((pos..pos, new_line));
        summary_parts.push("import Trace");
    }

    // Insert tracer field as the first body item. We add only the
    // leading newline + indent and trust the existing source to have
    // a newline after the opening `{` — that way we don't accumulate
    // blank lines if the source already had one.
    let tracer_line = format!(
        "\n{inner_indent}transient let tracer = {alias}.Tracer(Principal.fromActor(self));",
        alias = info.trace_alias
    );
    edits.push((info.insert_after_open..info.insert_after_open, tracer_line));
    summary_parts.push("`tracer` field");

    if !info.has_drain {
        // Insert just before the closing `}`. Trim back through any
        // trailing whitespace/newlines on the line above so we don't
        // pile up blank lines when the original source already ends
        // the actor body with one.
        let mut close = info.insert_before_close;
        let bytes = src.as_bytes();
        while close > 0 && matches!(bytes[close - 1], b' ' | b'\t' | b'\n') {
            close -= 1;
        }
        let drain = format!(
            "\n\n{inner_indent}public query func __debug_drain() : async Blob {{ tracer.drain() }};\n{actor_indent}"
        );
        edits.push((close..info.insert_before_close, drain));
        summary_parts.push("`__debug_drain`");
    }

    let summary = format!(
        "bootstrap actor: {}",
        summary_parts.join(" + ")
    );
    Some(Candidate {
        rule: Rule::MoBootstrap,
        byte_range: info.insert_after_open..info.insert_after_open,
        fn_name: "<actor>".to_string(),
        summary,
        warning: Some(
            "this rule must be accepted before mo-wrap-method takes effect — \
             without the `tracer` field, the inserted `tracer.beginTrace(…)` \
             calls will not compile."
                .to_string(),
        ),
        replacement: Replacement::Multi(edits),
    })
}

/// Try to resolve a sensible relative path from `path`'s directory to
/// the repo's `motoko/src/Trace.mo`. Falls back to a placeholder if no
/// such file is reachable by walking up.
fn guess_trace_import_path(path: &Path) -> String {
    let Some(start) = path.parent() else {
        return "../motoko/src/Trace".to_string();
    };
    let mut cursor: Option<&Path> = Some(start);
    let mut levels: usize = 0;
    while let Some(dir) = cursor {
        if dir.join("motoko/src/Trace.mo").exists() {
            let mut out = String::new();
            for _ in 0..levels {
                out.push_str("../");
            }
            out.push_str("motoko/src/Trace");
            return out;
        }
        levels += 1;
        if levels > 10 {
            break;
        }
        cursor = dir.parent();
    }
    "../motoko/src/Trace /* TODO: fix import path */".to_string()
}

fn last_import_end(src: &str) -> Option<usize> {
    let mut last = None;
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        if line.trim_start().starts_with("import ") {
            last = Some(offset + line.len());
        }
        offset += line.len();
    }
    last
}

fn leading_indent_of_line(src: &str, byte: usize) -> String {
    let line_start = src[..byte.min(src.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let bytes = src.as_bytes();
    let mut i = line_start;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    src[line_start..i].to_string()
}

fn rule_wrap_method(f: &PubFunc, info: &ActorInfo, src: &str) -> Option<Candidate> {
    if !info.has_tracer_field {
        return None; // bootstrap will handle this; revisit on next run
    }
    if !f.header_first {
        return None;
    }
    if f.has_begin_trace {
        return None;
    }

    Some(make_wrap_candidate(
        Rule::MoWrapMethod,
        f,
        info,
        src,
        format!(
            "wrap `{}` body with tracer.beginTrace / methodEntered / methodExited",
            f.name
        ),
        None,
        None,
    ))
}

fn rule_wrap_method_insert_header(
    f: &PubFunc,
    info: &ActorInfo,
    src: &str,
) -> Option<Candidate> {
    if !info.has_tracer_field {
        return None;
    }
    if f.header_first {
        return None;
    }
    if f.has_begin_trace {
        return None;
    }
    if f.is_query {
        // Mirror the Rust rule: queries are read-only, breaking them
        // for the sake of a TraceHeader is rarely worth it. Note that
        // `__debug_drain` is also excluded by this check.
        return None;
    }
    if f.name == "__debug_drain" {
        return None;
    }

    let alias_form = format!("?{}.TraceHeader", info.trace_alias);
    let header_param = if f.params_empty {
        format!("header : {alias_form}")
    } else {
        format!("header : {alias_form}, ")
    };
    Some(make_wrap_candidate(
        Rule::MoWrapMethodInsertHeader,
        f,
        info,
        src,
        format!(
            "wrap `{}` AND insert `header : {alias_form}` first param",
            f.name
        ),
        Some((f.params_open..f.params_open, header_param)),
        Some(
            "BREAKING ABI CHANGE: every caller of this method (including \
             agent-js scripts and other canisters) must be updated to pass \
             a TraceHeader as the first argument."
                .to_string(),
        ),
    ))
}

/// Build the body-boilerplate edit shared by Rules MoWrapMethod and
/// MoWrapMethodInsertHeader. `extra_edit`, when present, is spliced
/// alongside (used by Rule MoWrapMethodInsertHeader to also insert
/// the header parameter).
fn make_wrap_candidate(
    rule: Rule,
    f: &PubFunc,
    info: &ActorInfo,
    src: &str,
    summary: String,
    extra_edit: Option<(Range<usize>, String)>,
    warning: Option<String>,
) -> Candidate {
    // Indent: take the indentation of the line `body_open` lives on,
    // plus two spaces.
    let body_line_indent = leading_indent_of_line(src, f.body_open);
    let inner_indent = format!("{body_line_indent}  ");

    let caller_arg = if f.is_shared { "msg.caller" } else { "Principal.fromActor(self)" };
    let begin = format!(
        "\n{inner_indent}ignore {tracer}.beginTrace(header);\n{inner_indent}{tracer}.methodEntered(\"{name}\", {caller}, []);",
        tracer = info.tracer_ident,
        name = f.name,
        caller = caller_arg
    );
    let exited = format!(
        "\n{inner_indent}{tracer}.methodExited(null);",
        tracer = info.tracer_ident
    );
    // Insert `begin` right after the body's `{` so it's the first
    // statement.
    //
    // For `exited` the trick is the trailing expression: in Motoko,
    // the last value-producing expression in a body (no `;` after
    // it) is the function's return value. Inserting `methodExited`
    // *after* it would (a) silently change the return value to the
    // result of `methodExited` and (b) probably not even compile.
    // So: trim back through trailing whitespace; if the body's last
    // non-whitespace byte is `;`, the body has no trailing expression
    // and we insert there. Otherwise we find the last `;` (in code)
    // before the trailing expression and insert just after that.
    let body_after_open = f.body_open + 1;
    let body_close = f.body_range.end;
    let bytes = src.as_bytes();
    let body_str = &src[f.body_range.clone()];
    let body_mask = skip_mask(body_str);

    let mut tail = body_close;
    while tail > f.body_range.start
        && matches!(bytes[tail - 1], b' ' | b'\t' | b'\n')
    {
        tail -= 1;
    }

    let exit_pos = if tail > f.body_range.start && bytes[tail - 1] == b';' {
        // Last code character in body is `;` → no trailing return.
        tail
    } else if let Some(last_semi_local) = last_semicolon_in_code(body_str, &body_mask) {
        // Insert just after the last code-level `;`.
        f.body_range.start + last_semi_local + 1
    } else {
        // Single-expression body with no `;` anywhere — insert right
        // after the opening `{` so the trailing expression remains
        // the return value.
        body_after_open
    };

    let mut edits = vec![
        (body_after_open..body_after_open, begin),
        (exit_pos..exit_pos, exited),
    ];
    if let Some(extra) = extra_edit {
        edits.push(extra);
    }

    Candidate {
        rule,
        byte_range: f.body_open..f.body_open,
        fn_name: f.name.clone(),
        summary,
        warning,
        replacement: Replacement::Multi(edits),
    }
}

fn rule_entry_note(f: &PubFunc, info: &ActorInfo, _src: &str) -> Option<Candidate> {
    if !f.has_method_entered {
        return None;
    }
    if f.has_entry_note {
        return None;
    }
    // Insert immediately after `methodEntered(...)`. Find the actual
    // call's closing `)` followed by `;`.
    let body_str = &_src[f.body_range.clone()];
    let body_mask = skip_mask(body_str);
    let entered_pat = format!("{}.methodEntered(", info.tracer_ident);
    let entered_at = find_in_code(body_str, &body_mask, 0, &entered_pat)?;
    // Match the paren of the call and the trailing semi.
    let paren_open_in_body = entered_at + entered_pat.len() - 1;
    let paren_close_in_body = match_paren(body_str, &body_mask, paren_open_in_body)?;
    // Skip whitespace + ';'.
    let bytes = body_str.as_bytes();
    let mut after = paren_close_in_body + 1;
    while after < bytes.len() && (bytes[after] == b';' || bytes[after] == b' ') {
        after += 1;
    }
    if after < bytes.len() && bytes[after] == b'\n' {
        after += 1;
    }
    let insert_pos = f.body_range.start + after;
    let body_line_indent = leading_indent_of_line(_src, f.body_open);
    let inner_indent = format!("{body_line_indent}  ");
    let inserted = format!(
        "{inner_indent}{tracer}.note(\"{name}:enter\");\n",
        tracer = info.tracer_ident,
        name = f.name
    );
    Some(Candidate {
        rule: Rule::MoEntryNote,
        byte_range: insert_pos..insert_pos,
        fn_name: f.name.clone(),
        summary: format!(
            "emit {tracer}.note(\"{name}:enter\") after methodEntered",
            tracer = info.tracer_ident,
            name = f.name
        ),
        warning: None,
        replacement: Replacement::InsertRaw(inserted),
    })
}

fn rule_trap_note(f: &PubFunc, trap: &TrapCall, src: &str) -> Option<Candidate> {
    if !f.has_begin_trace {
        // Mirror the Rust trap-note rule: only fire inside an
        // already-traced method body.
        return None;
    }
    let line_start = src[..trap.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // Idempotency: check the previous non-empty line for an existing
    // `:trapped` note.
    let prev_line_end = if line_start == 0 { 0 } else { line_start - 1 };
    let prev_line_start = src[..prev_line_end]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let prev_line = &src[prev_line_start..prev_line_end];
    if prev_line.contains(":trapped\"") {
        return None;
    }
    let indent: String = src[line_start..trap.start]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let inserted = format!(
        "{indent}tracer.note(\"{name}:trapped\");\n",
        name = f.name
    );
    Some(Candidate {
        rule: Rule::MoTrapNote,
        byte_range: line_start..line_start,
        fn_name: f.name.clone(),
        summary: format!("emit tracer.note(\"{}:trapped\") before this Debug.trap", f.name),
        warning: None,
        replacement: Replacement::InsertRaw(inserted),
    })
}
