//! File-level parse helpers: scan `use` statements and build a map from
//! local idents to the canonical paths they refer to, so detection can
//! recognise `c(...)` after `use ic_cdk::call as c;`.

use std::collections::HashMap;
use syn::{File, Item, UseTree};

/// Maps local ident → canonical path, e.g. "c" → "ic_cdk::call".
/// Only tracks paths the wizard cares about (the ic-cdk call surface).
pub type AliasMap = HashMap<String, String>;

const TRACKED_PATHS: &[&[&str]] = &[
    &["ic_cdk", "call"],
    &["ic_cdk", "api", "call", "call_with_payment"],
    &["ic_cdk", "api", "call", "call_raw"],
    &["ic_cdk", "trap"],
    &["ic_cdk", "api", "trap"],
];

/// Scan top-level `use` statements and record any aliases for the paths
/// in `TRACKED_PATHS`. Both `use ic_cdk::call;` (binds `call`) and
/// `use ic_cdk::call as c;` (binds `c`) are recognised.
pub fn collect_aliases(file: &File) -> AliasMap {
    let mut out = AliasMap::new();
    for item in &file.items {
        if let Item::Use(u) = item {
            walk_use(&u.tree, &mut Vec::new(), &mut out);
        }
    }
    out
}

fn walk_use(tree: &UseTree, prefix: &mut Vec<String>, out: &mut AliasMap) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            walk_use(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(n) => {
            prefix.push(n.ident.to_string());
            if let Some(canonical) = match_tracked(prefix) {
                out.insert(n.ident.to_string(), canonical);
            }
            prefix.pop();
        }
        UseTree::Rename(r) => {
            prefix.push(r.ident.to_string());
            if let Some(canonical) = match_tracked(prefix) {
                out.insert(r.rename.to_string(), canonical);
            }
            prefix.pop();
        }
        UseTree::Group(g) => {
            for inner in &g.items {
                walk_use(inner, prefix, out);
            }
        }
        UseTree::Glob(_) => {
            // `use ic_cdk::*` could bring `call` into scope unqualified,
            // but it would also bring everything else; matching on a
            // bare `call(...)` after a glob is too risky for the
            // zero-false-positives bar. Skip.
        }
    }
}

fn match_tracked(segments: &[String]) -> Option<String> {
    for tracked in TRACKED_PATHS {
        if segments.len() == tracked.len()
            && segments.iter().zip(tracked.iter()).all(|(a, b)| a == b)
        {
            return Some(tracked.join("::"));
        }
    }
    None
}

/// Match a `syn::Path` against a list of canonical "::"-joined targets.
/// Bare single-segment paths (`call(...)`) are matched against the alias
/// map first; everything else against the full segment join.
pub fn path_matches(
    path: &syn::Path,
    aliases: &AliasMap,
    targets: &[&str],
) -> Option<String> {
    let joined: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let full = joined.join("::");

    if joined.len() == 1 {
        if let Some(canonical) = aliases.get(&joined[0]) {
            if targets.iter().any(|t| *t == canonical) {
                return Some(canonical.clone());
            }
        }
        return None;
    }
    if targets.iter().any(|t| *t == full) {
        return Some(full);
    }
    None
}

/// True if a `syn::Type` ends in a segment matching the given ident.
/// Used for detecting `TraceHeader` as a function parameter type
/// regardless of whether it's spelled `TraceHeader`,
/// `ic_debug_core::TraceHeader`, or `crate::TraceHeader`.
pub fn type_ends_with(ty: &syn::Type, ident: &str) -> bool {
    if let syn::Type::Path(tp) = ty {
        if let Some(last) = tp.path.segments.last() {
            return last.ident == ident;
        }
    }
    false
}
