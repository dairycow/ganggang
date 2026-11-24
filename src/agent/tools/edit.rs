//! `edit`: exact-match, all-or-nothing text replacement.
//!
//! Every edit must match exactly once in the file as of its turn; any miss
//! or ambiguity leaves the file untouched. The model reads the error and
//! retries with better anchors — that loop is the whole tool.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, display_relative, parse_args};

/// Files above this size are refused outright: editing a truncated view and
/// writing it back would silently destroy the tail.
const MAX_BYTES: usize = 512 * 1024;
const HUNK_PREVIEW_CAP: usize = 12;
const PREVIEW_LINE_CHARS: usize = 250;

fn cap_line(line: &str) -> String {
    if line.chars().count() <= PREVIEW_LINE_CHARS {
        line.to_string()
    } else {
        let cut: String = line.chars().take(PREVIEW_LINE_CHARS).collect();
        format!("{cut}…")
    }
}

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    edits: Vec<EditItem>,
}

#[derive(Deserialize)]
struct EditItem {
    old_text: String,
    new_text: String,
}

fn excerpt(s: &str) -> String {
    s.chars().take(80).collect()
}

fn line_number_at(content: &str, byte_pos: usize) -> usize {
    content[..byte_pos].bytes().filter(|&b| b == b'\n').count() + 1
}

fn match_line_numbers(content: &str, needle: &str, cap: usize) -> Vec<usize> {
    content
        .match_indices(needle)
        .take(cap)
        .map(|(pos, _)| line_number_at(content, pos))
        .collect()
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

fn nearest_match(content: &str, needle: &str) -> Option<(usize, usize)> {
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

fn format_nearest_context(content: &str, start: usize, w: usize) -> String {
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

fn format_hunk(start_line: usize, old_text: &str, new_text: &str) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let mut out = format!(
        "@@ line {start_line} (-{} +{}) @@",
        old_lines.len(),
        new_lines.len()
    );
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
    out
}

fn edit(cwd: &Path, path: &PathBuf, edits: &[EditItem]) -> ToolResult {
    if edits.is_empty() {
        return ToolResult::err("edits must not be empty".to_string());
    }
    // Bounded read (take caps the read itself — /dev/zero never EOFs).
    // Refuse above the cap rather than editing a truncated view: writing it
    // back would destroy the file's tail.
    let mut bytes = Vec::new();
    let loaded = std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(e) = loaded {
        return ToolResult::err(format!("cannot read {}: {e}", display_relative(cwd, path)));
    }
    if bytes.len() > MAX_BYTES {
        return ToolResult::err(format!(
            "refusing to edit {}: file exceeds the {} byte cap",
            display_relative(cwd, path),
            MAX_BYTES
        ));
    }
    let mut content = match String::from_utf8(bytes) {
        Ok(c) => c,
        Err(_) => {
            return ToolResult::err(format!(
                "cannot read {}: not valid UTF-8",
                display_relative(cwd, path)
            ));
        }
    };

    // All edits apply in memory first; the disk write happens only after
    // every match succeeded, so a late failure cannot leave a half-edited file.
    let mut hunks = Vec::with_capacity(edits.len());
    for (i, item) in edits.iter().enumerate() {
        if item.old_text.is_empty() {
            return ToolResult::err(format!("edit {} failed: old_text must not be empty", i + 1));
        }
        let hits = content.matches(&item.old_text).count();
        match hits {
            1 => {
                let start_line = content
                    .find(&item.old_text)
                    .map(|pos| line_number_at(&content, pos))
                    .unwrap_or(1);
                hunks.push(format_hunk(start_line, &item.old_text, &item.new_text));
                content = content.replacen(&item.old_text, &item.new_text, 1);
            }
            0 => {
                let mut msg = format!(
                    "edit {} failed: old_text not found: {:?}",
                    i + 1,
                    excerpt(&item.old_text)
                );
                if let Some((start, w)) = nearest_match(&content, &item.old_text) {
                    msg.push_str(&format!(
                        "\nclosest match around lines {}-{}:\n{}",
                        start + 1,
                        start + w,
                        format_nearest_context(&content, start, w)
                    ));
                }
                return ToolResult::err(msg);
            }
            n => {
                let mut lines = match_line_numbers(&content, &item.old_text, 10)
                    .into_iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                if n > 10 {
                    lines.push_str(", …");
                }
                return ToolResult::err(format!(
                    "edit {} failed: old_text found {n} times (lines {lines}; must be unique): {:?}",
                    i + 1,
                    excerpt(&item.old_text)
                ));
            }
        }
    }

    match std::fs::write(path, &content) {
        Ok(()) => ToolResult::ok(format!(
            "applied {} edit(s) to {}\n{}",
            edits.len(),
            display_relative(cwd, path),
            hunks.join("\n")
        )),
        Err(e) => ToolResult::err(format!("cannot write {}: {e}", display_relative(cwd, path))),
    }
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Apply exact-match edits: each old_text must appear exactly once, and any miss or \
ambiguity writes nothing."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit"},
                "edits": {
                    "type": "array",
                    "description": "Edits applied in order, atomically",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string", "description": "Exact text to replace; must be unique"},
                            "new_text": {"type": "string", "description": "Replacement text"}
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            // Bounded work: one file, matched and applied in memory. The
            // stop request is honored at the boundary — starting it is the
            // decision, and a half-applied edit is worse than a refused one.
            if cancel.is_cancelled() {
                return ToolResult::err("aborted by user".into());
            }
            let args: EditArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let full_path = self.cwd.join(&args.path);
            let cwd = self.cwd.clone();
            let edits = args.edits;
            tokio::task::spawn_blocking(move || edit(&cwd, &full_path, &edits))
                .await
                .unwrap_or_else(|e| ToolResult::err(format!("edit task failed: {e}")))
        })
    }
}

#[cfg(test)]
mod tests;
