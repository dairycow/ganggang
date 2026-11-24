//! Diagnostics and preview formatting for `edit`.
//!
//! The tool's value is largely in what it reports back: a miss names the
//! closest window and the first line where the text diverges, an ambiguity
//! names the matching lines, and a success shows a compact hunk. Those
//! helpers live here so the apply loop in `super` stays small.

const HUNK_PREVIEW_CAP: usize = 12;
const PREVIEW_LINE_CHARS: usize = 250;
/// Match line numbers listed in an ambiguity error before the ellipsis.
const AMBIGUOUS_LINE_CAP: usize = 10;

pub(super) fn cap_line(line: &str) -> String {
    if line.chars().count() <= PREVIEW_LINE_CHARS {
        line.to_string()
    } else {
        let cut: String = line.chars().take(PREVIEW_LINE_CHARS).collect();
        format!("{cut}…")
    }
}

pub(super) fn excerpt(s: &str) -> String {
    s.chars().take(80).collect()
}

pub(super) fn line_number_at(content: &str, byte_pos: usize) -> usize {
    content[..byte_pos].bytes().filter(|&b| b == b'\n').count() + 1
}

pub(super) fn match_line_numbers(content: &str, needle: &str, cap: usize) -> Vec<usize> {
    content
        .match_indices(needle)
        .take(cap)
        .map(|(pos, _)| line_number_at(content, pos))
        .collect()
}

/// The error for an `old_text` that matches more than once: where it matches,
/// and how to say which occurrence was meant.
pub(super) fn ambiguous_error(
    edit_no: usize,
    hits: usize,
    content: &str,
    old_text: &str,
) -> String {
    let mut lines = match_line_numbers(content, old_text, AMBIGUOUS_LINE_CAP)
        .into_iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if hits > AMBIGUOUS_LINE_CAP {
        lines.push_str(", …");
    }
    format!(
        "edit {edit_no} failed: old_text found {hits} times (lines {lines}; must be unique): {:?}\n\
         pass replace_all: true to replace all {hits} occurrences, or include more surrounding context",
        excerpt(old_text)
    )
}

fn token_set(s: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn line_similarity(hay: &str, hay_toks: &[&str], needle: &str, needle_toks: &[&str]) -> f64 {
    let ht = hay.trim();
    let nt = needle.trim();
    if ht.is_empty() && nt.is_empty() {
        return 0.0;
    }
    if hay == needle {
        return 1.0;
    }
    if !nt.is_empty() && ht == nt {
        return 0.9;
    }
    let mut best = 0.0f64;
    if !nt.is_empty() && !ht.is_empty() && (ht.contains(nt) || nt.contains(ht)) {
        let min_len = ht.len().min(nt.len()) as f64;
        let max_len = ht.len().max(nt.len()) as f64;
        best = best.max(0.65 * (min_len / max_len));
    }
    if !hay_toks.is_empty() && !needle_toks.is_empty() {
        let common = hay_toks
            .iter()
            .filter(|t| needle_toks.binary_search(t).is_ok())
            .count();
        let dice = (2.0 * common as f64) / (hay_toks.len() + needle_toks.len()) as f64;
        best = best.max(dice);
    }
    best
}

pub(super) fn nearest_match(content: &str, needle: &str) -> Option<(usize, usize)> {
    let hay_lines: Vec<&str> = content.lines().collect();
    let needle_lines: Vec<&str> = needle.lines().collect();
    if hay_lines.is_empty() || needle_lines.is_empty() {
        return None;
    }
    let hay_tokens: Vec<Vec<&str>> = hay_lines.iter().map(|l| token_set(l.trim())).collect();
    let needle_tokens: Vec<Vec<&str>> = needle_lines.iter().map(|l| token_set(l.trim())).collect();
    let w = needle_lines.len().min(hay_lines.len());
    let mut best_start = 0usize;
    let mut best_score = 0.0f64;
    for start in 0..=hay_lines.len() - w {
        let sum: f64 = (0..w)
            .map(|k| {
                line_similarity(
                    hay_lines[start + k],
                    &hay_tokens[start + k],
                    needle_lines[k],
                    &needle_tokens[k],
                )
            })
            .sum();
        let avg = sum / w as f64;
        if avg > best_score {
            best_score = avg;
            best_start = start;
        }
    }
    (best_score >= 0.25).then_some((best_start, w))
}

pub(super) fn format_nearest_context(content: &str, start: usize, w: usize) -> String {
    let hay_lines: Vec<&str> = content.lines().collect();
    let ctx_start = start.saturating_sub(3);
    let ctx_end = (start + w + 3).min(hay_lines.len()).min(ctx_start + 30);
    hay_lines[ctx_start..ctx_end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}| {}", ctx_start + i + 1, cap_line(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line naming where the needle first diverges from the window
/// `nearest_match` picked, flagged when the difference is indentation alone.
pub(super) fn first_difference(
    content: &str,
    needle: &str,
    start: usize,
    w: usize,
) -> Option<String> {
    let hay_lines: Vec<&str> = content.lines().collect();
    let needle_lines: Vec<&str> = needle.lines().collect();
    for k in 0..w {
        let (Some(want), Some(got)) = (needle_lines.get(k), hay_lines.get(start + k)) else {
            return None;
        };
        if want == got {
            continue;
        }
        let note = if want.trim() == got.trim() {
            " (whitespace differs)"
        } else {
            ""
        };
        return Some(format!(
            "\nfirst difference at line {}{note}: expected {:?} vs actual {:?}",
            start + k + 1,
            cap_line(want),
            cap_line(got)
        ));
    }
    None
}

/// The `-`/`+` preview lines of a hunk, capped so a large edit stays short.
fn push_preview(out: &mut String, old_lines: &[&str], new_lines: &[&str]) {
    let old_cap = (HUNK_PREVIEW_CAP / 2).max(HUNK_PREVIEW_CAP.saturating_sub(new_lines.len()));
    let old_show = old_lines.len().min(old_cap);
    let new_show = new_lines
        .len()
        .min(HUNK_PREVIEW_CAP.saturating_sub(old_show));
    for line in &old_lines[..old_show] {
        out.push_str(&format!("\n-{}", cap_line(line)));
    }
    for line in &new_lines[..new_show] {
        out.push_str(&format!("\n+{}", cap_line(line)));
    }
    let total = old_lines.len() + new_lines.len();
    let shown = old_show + new_show;
    if total > shown {
        out.push_str(&format!("\n... (+{} more lines)", total - shown));
    }
}

pub(super) fn format_hunk(start_line: usize, old_text: &str, new_text: &str) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut out = format!(
        "@@ line {start_line} (-{} +{}) @@",
        old_lines.len(),
        new_lines.len()
    );
    push_preview(&mut out, &old_lines, &new_lines);
    out
}

/// One hunk for a `replace_all: true` edit: the header names the occurrence
/// count, and the body previews the replacement once.
pub(super) fn format_multi_occurrence_hunk(
    start_line: usize,
    hits: usize,
    old_text: &str,
    new_text: &str,
) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut out = format!(
        "@@ line {start_line} ({hits} occurrence(s), -{} +{}) @@",
        old_lines.len(),
        new_lines.len()
    );
    push_preview(&mut out, &old_lines, &new_lines);
    out
}
