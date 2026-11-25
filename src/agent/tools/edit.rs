//! `edit`: exact-match, all-or-nothing text replacement.
//!
//! Every edit must match exactly once in the file as of its turn; any miss
//! or ambiguity leaves the file untouched. The model reads the error and
//! retries with better anchors — that loop is the whole tool. An edit that
//! sets `replace_all: true` opts out of the uniqueness requirement and
//! rewrites every non-overlapping occurrence.

mod diagnostics;

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, display_relative, parse_args};
use diagnostics::{
    ambiguous_error, excerpt, first_difference, format_hunk, format_multi_occurrence_hunk,
    format_nearest_context, line_number_at, nearest_match,
};

/// Files above this size are refused outright: editing a truncated view and
/// writing it back would silently destroy the tail.
const MAX_BYTES: usize = 512 * 1024;

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<EditItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditItem {
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
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
                    if let Some(diff) = first_difference(&content, &item.old_text, start, w) {
                        msg.push_str(&diff);
                    }
                }
                return ToolResult::err(msg);
            }
            n if item.replace_all => {
                let start_line = content
                    .find(&item.old_text)
                    .map(|pos| line_number_at(&content, pos))
                    .unwrap_or(1);
                hunks.push(format_multi_occurrence_hunk(
                    start_line,
                    n,
                    &item.old_text,
                    &item.new_text,
                ));
                content = content.replace(&item.old_text, &item.new_text);
            }
            n => return ToolResult::err(ambiguous_error(i + 1, n, &content, &item.old_text)),
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
        "Apply exact-match edits: each old_text must appear exactly once unless the edit sets \
replace_all: true, and any miss or ambiguity writes nothing."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit, relative to the cwd, absolute, or starting with ~"},
                "edits": {
                    "type": "array",
                    "description": "Edits applied in order, atomically",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string", "description": "Exact text to replace; must be unique"},
                            "new_text": {"type": "string", "description": "Replacement text"},
                            "replace_all": {"type": "boolean", "description": "Replace every non-overlapping occurrence of old_text instead of requiring a unique match (default: false)"}
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
            let full_path = super::resolve_path(&self.cwd, &args.path);
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
