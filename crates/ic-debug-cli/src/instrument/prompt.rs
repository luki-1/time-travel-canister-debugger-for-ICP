//! Interactive prompt loop. Each candidate is presented with a few lines
//! of source context; the user accepts, declines, skips the rest of the
//! file, or quits. Rule 4 (snapshot a constructed local) additionally
//! asks for a key string.

use anyhow::Result;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use super::{Candidate, Replacement};

pub fn run_loop(candidates: &[Candidate], src: &str) -> Result<Vec<Candidate>> {
    println!(
        "found {} candidate(s). For each, choose: accept / decline / skip-rest / quit.\n",
        candidates.len()
    );
    let theme = ColorfulTheme::default();
    let mut accepted: Vec<Candidate> = Vec::new();
    let mut skip_rest = false;

    for (i, c) in candidates.iter().enumerate() {
        if skip_rest {
            break;
        }
        print_candidate_header(i + 1, candidates.len(), c, src);

        let choice = Select::with_theme(&theme)
            .items(&["accept", "decline", "skip rest of file", "quit"])
            .default(0)
            .interact()?;

        match choice {
            0 => {
                let mut accepted_c = c.clone();
                match &c.replacement {
                    Replacement::InsertAfterWithKey {
                        template,
                        default_key,
                    } => {
                        let key: String = Input::with_theme(&theme)
                            .with_prompt("snapshot key")
                            .default(default_key.clone())
                            .interact_text()?;
                        let key = key.trim().to_string();
                        let key = if key.is_empty() {
                            default_key.clone()
                        } else {
                            key
                        };
                        accepted_c.replacement =
                            Replacement::InsertRaw(template.replace("{KEY}", &key));
                    }
                    Replacement::InsertWithKeyAndValue {
                        template,
                        default_key,
                        value_hint,
                    } => {
                        let key: String = Input::with_theme(&theme)
                            .with_prompt("snapshot key")
                            .default(default_key.clone())
                            .interact_text()?;
                        let key = key.trim().to_string();
                        let key = if key.is_empty() {
                            default_key.clone()
                        } else {
                            key
                        };
                        let value: String = Input::with_theme(&theme)
                            .with_prompt(format!(
                                "value expression (suggested: `{value_hint}`)"
                            ))
                            .default(value_hint.clone())
                            .interact_text()?;
                        let value = value.trim().to_string();
                        let value = if value.is_empty() {
                            value_hint.clone()
                        } else {
                            value
                        };
                        accepted_c.replacement = Replacement::InsertRaw(
                            template
                                .replace("{KEY}", &key)
                                .replace("{VALUE}", &value),
                        );
                    }
                    _ => {} // Replace / InsertRaw / Multi need no further input.
                }
                accepted.push(accepted_c);
            }
            1 => {} // decline
            2 => skip_rest = true,
            _ => return Ok(Vec::new()), // quit ⇒ no edits applied
        }
    }
    Ok(accepted)
}

pub fn confirm_apply() -> Result<bool> {
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("apply these edits to the file?")
        .default(false)
        .interact()?)
}

pub fn confirm_apply_agent_js(count: usize) -> Result<bool> {
    Ok(Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(format!(
            "apply {count} agent-js edit(s) to keep callers in sync?"
        ))
        .default(false)
        .interact()?)
}

fn print_candidate_header(i: usize, total: usize, c: &Candidate, src: &str) {
    let line = line_of_offset(src, c.byte_range.start);
    println!();
    println!(
        "── [{}/{}] {} (line {}, fn `{}`)",
        i,
        total,
        c.rule.as_str(),
        line,
        c.fn_name
    );
    println!("   {}", c.summary);
    if let Some(w) = &c.warning {
        println!("   ⚠ {w}");
    }
    println!("   ┄┄┄ source ┄┄┄");
    print_context(src, c.byte_range.start, 3);
}

fn print_context(src: &str, offset: usize, around: usize) {
    let target_line = line_of_offset(src, offset);
    let lo = target_line.saturating_sub(around);
    let hi = target_line + around;
    for (i, line) in src.lines().enumerate() {
        let ln = i + 1;
        if ln < lo {
            continue;
        }
        if ln > hi {
            break;
        }
        let marker = if ln == target_line { "▶" } else { " " };
        println!("   {marker} {ln:>4} │ {line}");
    }
}

fn line_of_offset(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count() + 1
}
