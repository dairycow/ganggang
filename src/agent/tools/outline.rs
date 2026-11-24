//! The outline tool: a file's structure as a short list of signatures.
//!
//! Reading a whole module costs tokens the model often does not need, and
//! grep only finds a name once you already know it. This tool answers "what
//! is in this file, and on which lines" for a bounded cost: one line per
//! symbol, each with the 1-based range to pass to `read(offset, limit)`.
//! The extraction itself lives in [`lang`], one small scanner per language
//! family; everything here is the bounded read and the rendering.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, Cancel, Tool, ToolResult, display_relative, parse_args};

mod brace;
mod lang;
#[cfg(test)]
mod tests;

/// The largest file outlined. Anything bigger is read in ranges instead.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Entries returned when the model does not ask for a different number.
const DEFAULT_MAX_ENTRIES: usize = 200;
/// The most entries the tool will ever return.
const MAX_ENTRIES: usize = 500;
/// The largest rendered outline, whatever the entry cap allows.
const MAX_OUTPUT_BYTES: usize = 12 * 1024;
/// Signatures longer than this are cut with an ellipsis.
const MAX_SIG_CHARS: usize = 140;

pub struct OutlineTool {
    cwd: PathBuf,
}

impl OutlineTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    max_entries: Option<usize>,
}

impl Tool for OutlineTool {
    fn name(&self) -> &'static str {
        "outline"
    }

    fn description(&self) -> &'static str {
        "Return a compact structural outline of a file: symbol or heading \
         signatures with 1-based line ranges, so you can jump straight to \
         read(offset, limit). Supports Rust, Python, JS/TS, Go, C/C++/Java/C#, \
         shell, Markdown and TOML."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the working directory" },
                "max_entries": { "type": "integer", "description": "Maximum entries to return (default 200, max 500)" },
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        let cwd = self.cwd.clone();
        let args = match parse_args::<Args>(args) {
            Ok(args) => args,
            Err(e) => return Box::pin(async move { ToolResult::err(e) }),
        };
        Box::pin(async move {
            if cancel.is_cancelled() {
                return ToolResult::err("aborted by user".into());
            }
            let max = args
                .max_entries
                .unwrap_or(DEFAULT_MAX_ENTRIES)
                .min(MAX_ENTRIES);
            let path = cwd.join(&args.path);
            let cancel = cancel.clone();
            let outlined =
                tokio::task::spawn_blocking(move || outline(&path, &cwd, max, &cancel)).await;
            match outlined {
                Ok(Ok(text)) => ToolResult::ok(text),
                Ok(Err(msg)) => ToolResult::err(msg),
                Err(e) => ToolResult::err(format!("outline failed: {e}")),
            }
        })
    }
}

/// Read the file under a hard byte cap, extract, and render. Every refusal
/// is a message the model can act on, not a Rust error.
fn outline(path: &Path, cwd: &Path, max: usize, cancel: &Cancel) -> Result<String, String> {
    if !path.exists() {
        return Err(format!("no such file: {}", display_relative(cwd, path)));
    }
    let file = File::open(path).map_err(|e| format!("{}: {e}", display_relative(cwd, path)))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", display_relative(cwd, path)))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "file is larger than {MAX_FILE_BYTES} bytes; use read(offset, limit) or grep instead"
        ));
    }
    if bytes.contains(&0) {
        return Err(format!(
            "{} looks like a binary file; outline reads text only",
            display_relative(cwd, path)
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{} is not valid UTF-8", display_relative(cwd, path)))?;
    let total_lines = text.lines().count();
    let entries =
        lang::extract(path, &text, cancel).ok_or_else(|| "aborted by user".to_string())?;
    Ok(render(path, cwd, total_lines, &entries, max))
}

/// A signature, trimmed, its opening brace dropped, capped with an ellipsis.
fn signature(sig: &str) -> String {
    let trimmed = sig.trim().trim_end_matches('{').trim_end();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= MAX_SIG_CHARS {
        return trimmed.to_string();
    }
    let mut cut: String = chars[..MAX_SIG_CHARS - 1].iter().collect();
    cut.push('…');
    cut
}

/// One entry as `range: signature`, indented by how deeply it is nested.
fn entry_line(entry: &lang::Entry) -> String {
    let indent = "  ".repeat(entry.depth.min(2));
    let range = if entry.end > entry.start {
        format!("{}-{}", entry.start, entry.end)
    } else {
        entry.start.to_string()
    };
    let sig = signature(&entry.sig);
    format!("{indent}{range}: {sig}\n")
}

/// The header line, then one line per entry, then the stop notice if any
/// entries were left out, by count or by byte cap.
fn render(
    path: &Path,
    cwd: &Path,
    total_lines: usize,
    entries: &[lang::Entry],
    max: usize,
) -> String {
    let mut out = format!("{} ({} lines)\n", display_relative(cwd, path), total_lines);
    if entries.is_empty() {
        out.push_str(&format!(
            "(no symbols found in {total_lines} lines; use read)\n"
        ));
        return out;
    }
    let shown = entries.len().min(max);
    let mut printed = 0usize;
    for entry in &entries[..shown] {
        let line = entry_line(entry);
        if out.len() + line.len() > MAX_OUTPUT_BYTES {
            break;
        }
        out.push_str(&line);
        printed += 1;
    }
    if printed < shown {
        out.push_str(&format!(
            "[truncated: output capped at {MAX_OUTPUT_BYTES} bytes]\n"
        ));
    } else if shown < entries.len() {
        out.push_str(&format!(
            "[truncated: showing {shown} of {} entries]\n",
            entries.len()
        ));
    }
    out
}
