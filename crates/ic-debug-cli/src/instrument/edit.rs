//! Apply accepted candidates to source text.
//!
//! Edits are byte-range based. We apply them right-to-left so earlier
//! offsets stay valid; conflicts (overlapping ranges) are rejected
//! up-front rather than silently merged.

use std::ops::Range;

use anyhow::{bail, Result};

use super::{Candidate, Replacement, Rule};

/// One concrete edit, after Multi candidates have been flattened.
struct FlatEdit {
    rule: Rule,
    range: Range<usize>,
    text: String,
}

pub fn apply(src: &str, accepted: &[Candidate]) -> Result<String> {
    let mut edits: Vec<FlatEdit> = Vec::new();
    for c in accepted {
        flatten(c, &mut edits);
    }
    edits.sort_by_key(|e| e.range.start);

    // Reject overlapping non-zero-width ranges. Zero-width inserts at the
    // same offset are allowed (each one inserts a separate line).
    for w in edits.windows(2) {
        let a = &w[0].range;
        let b = &w[1].range;
        let a_zero = a.start == a.end;
        let b_zero = b.start == b.end;
        if !(a_zero && b_zero) && a.end > b.start {
            bail!(
                "conflicting edits: {} ({}..{}) overlaps {} ({}..{})",
                w[0].rule.as_str(),
                a.start,
                a.end,
                w[1].rule.as_str(),
                b.start,
                b.end,
            );
        }
    }

    let mut out = src.to_string();
    for e in edits.iter().rev() {
        out.replace_range(e.range.clone(), &e.text);
    }
    Ok(out)
}

fn flatten(c: &Candidate, out: &mut Vec<FlatEdit>) {
    match &c.replacement {
        Replacement::Replace(s) | Replacement::InsertRaw(s) => out.push(FlatEdit {
            rule: c.rule,
            range: c.byte_range.clone(),
            text: s.clone(),
        }),
        Replacement::InsertAfterWithKey { template, default_key } => out.push(FlatEdit {
            rule: c.rule,
            range: c.byte_range.clone(),
            text: template.replace("{KEY}", default_key),
        }),
        Replacement::InsertWithKeyAndValue {
            template,
            default_key,
            value_hint,
        } => out.push(FlatEdit {
            rule: c.rule,
            range: c.byte_range.clone(),
            // Diff-only / dry-run path: fall back to the suggested key
            // and value hint so the diff is still compilable.
            text: template
                .replace("{KEY}", default_key)
                .replace("{VALUE}", value_hint),
        }),
        Replacement::Multi(edits) => {
            for (range, text) in edits {
                out.push(FlatEdit {
                    rule: c.rule,
                    range: range.clone(),
                    text: text.clone(),
                });
            }
        }
    }
}
