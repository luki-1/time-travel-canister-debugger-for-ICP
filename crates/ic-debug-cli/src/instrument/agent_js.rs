//! Light-touch agent-js update: when Rule 1b inserts a `TraceHeader`
//! parameter into a canister method, this module scans `.mjs` / `.js`
//! files under a configurable root for two specific patterns whose
//! shape we can reliably recognise:
//!
//!   1. `<method>: IDL.Func([…], …)` — the Candid IDL stub. We prepend
//!      `Header,` to the argument list if it isn't there already.
//!   2. `<receiver>.<method>(<args>)` — an actor call. We prepend
//!      `trace.header(), ` to the argument list if it isn't there
//!      already.
//!
//! Both patterns are regex-based and intentionally conservative — we
//! refuse to edit anything we can't fully match. No JS parser dep.
//! False positives are possible (a method name might collide with an
//! unrelated property access) so each proposed edit is presented to
//! the user for individual review, exactly like the Rust-side flow.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Reference {
    pub kind: RefKind,
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
    /// Byte offset within `line_text` where the inserted text should go.
    pub insert_col: usize,
    /// Text to insert at `(line_no, insert_col)`.
    pub insertion: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    IdlFunc,
    ActorCall,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::IdlFunc => "IDL.Func",
            RefKind::ActorCall => "actor call",
        }
    }
}

/// Walk `root` for `.mjs` / `.js` files and return every reference to
/// `method` that matches one of the two recognised patterns. Files
/// already showing the `Header` / `trace.header()` shape on the
/// matched line are skipped — re-running the tool is a no-op.
pub fn find_references(method: &str, root: &Path) -> Result<Vec<Reference>> {
    let mut files: Vec<PathBuf> = Vec::new();
    walk_js(root, &mut files)?;
    files.sort();

    let mut out = Vec::new();
    for path in files {
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue, // unreadable / non-utf8: skip, don't fail
        };
        for (i, line) in src.lines().enumerate() {
            if let Some(r) = match_idl_func(line, method, &path, i + 1) {
                out.push(r);
            }
            if let Some(r) = match_actor_call(line, method, &path, i + 1) {
                out.push(r);
            }
        }
    }
    Ok(out)
}

fn walk_js(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(()); // missing root is fine; user simply has no agent-js dir
    }
    for entry in
        fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.')
                || matches!(name.as_ref(), "node_modules" | "dist" | "build")
            {
                continue;
            }
            walk_js(&path, out)?;
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "mjs" | "js" | "cjs" | "ts") {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Match `<method>: IDL.Func([<args>], …)`. We require the colon, the
/// exact `IDL.Func(` literal, and the `[`; this rules out almost every
/// false positive we can think of — JS objects rarely have a key
/// followed by `IDL.Func(` outside a Candid IDL definition.
fn match_idl_func(
    line: &str,
    method: &str,
    path: &Path,
    line_no: usize,
) -> Option<Reference> {
    let needle = format!("{method}:");
    let key_pos = line.find(&needle)?;
    // Look for `IDL.Func([` somewhere after the key.
    let rest = &line[key_pos + needle.len()..];
    let func_pos = rest.find("IDL.Func(")?;
    let after_func = &rest[func_pos + "IDL.Func(".len()..];
    let bracket_pos = after_func.find('[')?;
    let args_start = key_pos + needle.len() + func_pos + "IDL.Func(".len() + bracket_pos + 1;

    // Idempotency: if the args list already starts with `Header`,
    // possibly preceded by whitespace, skip.
    let args_tail = &line[args_start..];
    let trimmed = args_tail.trim_start();
    if trimmed.starts_with("Header,") || trimmed.starts_with("Header ]") || trimmed.starts_with("Header]") {
        return None;
    }

    Some(Reference {
        kind: RefKind::IdlFunc,
        path: path.to_path_buf(),
        line_no,
        line_text: line.to_string(),
        insert_col: args_start,
        insertion: "Header, ".to_string(),
    })
}

/// Match `<receiver>.<method>(<args>)`. We require a `.` immediately
/// before the method name and a `(` immediately after; this rules out
/// `foo_method` (substring) and `methodOther` (suffix).
fn match_actor_call(
    line: &str,
    method: &str,
    path: &Path,
    line_no: usize,
) -> Option<Reference> {
    let needle = format!(".{method}(");
    let pos = line.find(&needle)?;
    // Verify the char before the dot is part of an identifier or `]`
    // (e.g. `actor[i].method(`) — i.e. the dot is a member access, not
    // some line-continuation `.method(` orphan.
    if pos == 0 {
        return None;
    }
    let prev = line[..pos].chars().rev().next().unwrap();
    if !(prev.is_alphanumeric() || prev == '_' || prev == ']' || prev == ')') {
        return None;
    }

    let args_start = pos + needle.len();
    // Idempotency: skip if `trace.header()` is already the first arg.
    let args_tail = &line[args_start..];
    let trimmed = args_tail.trim_start();
    if trimmed.starts_with("trace.header()")
        || trimmed.starts_with("header()")
        || trimmed.starts_with("trace_header(")
    {
        return None;
    }

    Some(Reference {
        kind: RefKind::ActorCall,
        path: path.to_path_buf(),
        line_no,
        line_text: line.to_string(),
        insert_col: args_start,
        insertion: "trace.header(), ".to_string(),
    })
}

/// Apply a list of references to their files atomically per-file.
/// References are grouped by path, sorted by line/col descending, and
/// applied in place. Returns the number of edits actually written.
pub fn apply(references: &[Reference]) -> Result<usize> {
    use std::collections::HashMap;
    let mut by_path: HashMap<PathBuf, Vec<&Reference>> = HashMap::new();
    for r in references {
        by_path.entry(r.path.clone()).or_default().push(r);
    }
    let mut edits = 0usize;
    for (path, mut refs) in by_path {
        // Sort descending by (line, col) so earlier offsets stay valid
        // when we splice text in line by line.
        refs.sort_by(|a, b| b.line_no.cmp(&a.line_no).then(b.insert_col.cmp(&a.insert_col)));
        let src = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let mut lines: Vec<String> = src.lines().map(|l| l.to_string()).collect();
        let trailing_newline = src.ends_with('\n');
        for r in refs {
            if r.line_no == 0 || r.line_no > lines.len() {
                continue;
            }
            let line = &mut lines[r.line_no - 1];
            if r.insert_col > line.len() {
                continue;
            }
            line.insert_str(r.insert_col, &r.insertion);
            edits += 1;
        }
        let mut out = lines.join("\n");
        if trailing_newline {
            out.push('\n');
        }
        fs::write(&path, out)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reffrom(line: &str, method: &str) -> Vec<Reference> {
        let mut out = Vec::new();
        if let Some(r) = match_idl_func(line, method, Path::new("x.mjs"), 1) {
            out.push(r);
        }
        if let Some(r) = match_actor_call(line, method, Path::new("x.mjs"), 1) {
            out.push(r);
        }
        out
    }

    #[test]
    fn matches_idl_func() {
        let line = "  lock_funds: IDL.Func([IDL.Nat64, IDL.Nat64], [Lock], []),";
        let r = reffrom(line, "lock_funds");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, RefKind::IdlFunc);
    }

    #[test]
    fn skips_idl_func_when_header_already_first() {
        let line = "  lock_funds: IDL.Func([Header, IDL.Nat64], [Lock], []),";
        let r = reffrom(line, "lock_funds");
        assert!(r.iter().all(|r| r.kind != RefKind::IdlFunc), "{:?}", r);
    }

    #[test]
    fn matches_actor_call() {
        let line = "  await escrow.lock_funds(payment_id, 100n);";
        let r = reffrom(line, "lock_funds");
        let calls: Vec<_> = r.iter().filter(|r| r.kind == RefKind::ActorCall).collect();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn skips_actor_call_when_header_already_present() {
        let line = "  await escrow.lock_funds(trace.header(), payment_id, 100n);";
        let r = reffrom(line, "lock_funds");
        assert!(r.iter().all(|r| r.kind != RefKind::ActorCall), "{:?}", r);
    }

    #[test]
    fn does_not_match_substring() {
        // `lock_funds_v2` should not match `lock_funds`.
        let line = "  lock_funds_v2: IDL.Func([IDL.Nat64], [Lock], []),";
        let r = reffrom(line, "lock_funds");
        assert!(r.is_empty(), "{:?}", r);
    }

    #[test]
    fn does_not_match_dot_method_at_line_start() {
        let line = ".lock_funds(payment_id);";
        let r = reffrom(line, "lock_funds");
        assert!(
            r.iter().all(|r| r.kind != RefKind::ActorCall),
            "got {:?}",
            r
        );
    }
}
