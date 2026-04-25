//! Render a unified diff for the user's final review.

use std::path::Path;

use similar::{ChangeTag, TextDiff};

pub fn unified(old: &str, new: &str, path: &Path) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    out.push_str(&format!("--- {}\n+++ {}\n", path.display(), path.display()));
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        out.push_str(&format!("{}", hunk.header()));
        for change in hunk.iter_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            out.push_str(&format!("{sign}{}", change.value()));
        }
    }
    out
}
