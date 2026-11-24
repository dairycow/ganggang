//! `read`: text file → numbered lines, with hard output caps.
//!
//! Streams sequentially so `offset` pages cleanly past 50 KiB without
//! loading or truncating the whole file first, while a scan cap keeps
//! endless files like `/dev/zero` bounded. A negative `offset` reads the
//! last N lines instead.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, parse_args};

mod scan;
mod tail;
#[cfg(test)]
mod tests;

use scan::read;
use tail::read_tail;

/// Default page when `limit` is omitted.
const DEFAULT_LINES: usize = 400;
/// Hard ceiling for a page when `limit` is passed.
const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 24 * 1024;
const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
/// A single minified line cannot consume the whole page.
const MAX_LINE_CHARS: usize = 500;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        self.cwd.join(path)
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    /// 1-based line to start at; negative reads the last N lines.
    offset: Option<isize>,
    /// Number of lines to return.
    limit: Option<usize>,
}

/// The longest valid UTF-8 prefix of `s` that fits `max_bytes`.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_bytes)
        .last()
        .unwrap_or(0)
}

/// Cut a line to `MAX_LINE_CHARS` characters, marking the cut with `…`.
fn cap_line(text: &str) -> String {
    if text.chars().count() <= MAX_LINE_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MAX_LINE_CHARS).collect();
    cut.push('…');
    cut
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a text file as 1-based numbered lines (N| text). Returns up to 400 lines or \
 24 KiB by default, with truncation noted; an explicit limit caps at 2000 lines. A negative \
 offset reads the last N lines. Lines over 500 characters are cut."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory"},
                "offset": {"type": "integer", "description": "1-based line to start at; negative reads the last N lines"},
                "limit": {"type": "integer", "description": "Maximum lines to return (default 400, max 2000)"}
            },
            "required": ["path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return ToolResult::err("aborted by user".into());
            }
            let args: ReadArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let path = self.resolve(&args.path);
            let offset = args.offset;
            let limit = args.limit;
            let cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || match offset {
                Some(o) if o < 0 => read_tail(&path, o.unsigned_abs().min(MAX_LINES), &cancel),
                _ => read(&path, offset, limit, &cancel),
            })
            .await
            {
                Ok(res) => res,
                Err(e) => ToolResult::err(format!("read task failed: {e}")),
            }
        })
    }
}
