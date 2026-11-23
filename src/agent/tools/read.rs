//! `read`: text file → string, with hard output caps.
//!
//! The caps exist so one giant file cannot eat the context window: at most
//! 2000 lines and 50 KiB, with the truncation visible to the model.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Tool, ToolResult, parse_args};

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 50 * 1024;

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
    /// 1-based line to start at.
    offset: Option<usize>,
    /// Number of lines to return.
    limit: Option<usize>,
}

fn read(path: &Path, offset: Option<usize>, limit: Option<usize>) -> ToolResult {
    // Read at most MAX_BYTES + 1 (the +1 proves truncation): read_to_string
    // would buffer the whole file first, and an endless "file" like
    // /dev/zero never EOFs.
    let mut bytes = Vec::new();
    let loaded = std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(e) = loaded {
        return ToolResult::err(format!("cannot read {}: {e}", path.display()));
    }
    // The +1 probe is the truncation signal: by the time the string exists
    // it is already within the cap, so the byte count decides the notice.
    let hit_byte_cap = bytes.len() > MAX_BYTES;
    let content = match std::str::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        // An incomplete char at the tail is OUR cut, not the file's fault:
        // keep the valid prefix. Interior invalid bytes are the file's.
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        Err(_) => {
            return ToolResult::err(format!("cannot read {}: not valid UTF-8", path.display()));
        }
    };

    let total_lines = content.lines().count();
    let start = offset.unwrap_or(1).saturating_sub(1);
    let window: Vec<&str> = content
        .lines()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    let mut lines = window.clone();
    let line_capped = lines.len() > MAX_LINES;
    if line_capped {
        lines.truncate(MAX_LINES);
    }
    let mut out = lines.join("\n");

    // Cut at the last char boundary within the byte cap, so slicing never
    // splits a UTF-8 sequence.
    let byte_capped = hit_byte_cap || out.len() > MAX_BYTES;
    if out.len() > MAX_BYTES {
        let cut = out
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= MAX_BYTES)
            .last()
            .unwrap_or(0);
        out.truncate(cut);
    }

    // An explicitly requested offset/limit is not truncation — the model
    // already knows the window. Only announce what WE cut.
    let range_capped = limit.is_none() && start + window.len() < total_lines;
    if line_capped || byte_capped || range_capped {
        let mut notes = Vec::new();
        if line_capped || range_capped {
            notes.push(format!("showing {} of {total_lines} lines", lines.len()));
        }
        if byte_capped {
            notes.push(format!("byte cap {MAX_BYTES} reached"));
        }
        out = format!("{out}\n[truncated: {}]", notes.join("; "));
    }

    ToolResult::ok(out)
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a text file. Returns up to 2000 lines / 50 KiB; truncated reads say so. \
Use offset (1-based line) and limit to page through larger files."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path, relative to the working directory"},
                "offset": {"type": "integer", "description": "1-based line to start at"},
                "limit": {"type": "integer", "description": "Number of lines to return"}
            },
            "required": ["path"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: ReadArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            read(&self.resolve(&args.path), args.offset, args.limit)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
        ReadTool::new(dir.to_path_buf()).execute(&args).await
    }

    #[tokio::test]
    async fn reads_a_file_and_respects_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();

        let r = run(dir.path(), serde_json::json!({"path": "f.txt"})).await;
        assert!(!r.is_error);
        assert_eq!(r.output.trim(), "a\nb\nc\nd");

        let r = run(
            dir.path(),
            serde_json::json!({"path": "f.txt", "offset": 2, "limit": 2}),
        )
        .await;
        assert_eq!(r.output.trim(), "b\nc");
    }

    #[tokio::test]
    async fn truncates_at_2000_lines_with_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..2500).map(|i| format!("line{i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), body).unwrap();

        let r = run(dir.path(), serde_json::json!({"path": "big.txt"})).await;
        assert!(!r.is_error);
        assert!(r.output.contains("line1999"));
        assert!(!r.output.contains("\nline2000\n"));
        assert!(r.output.contains("[truncated: showing 2000 of 2500 lines]"));
    }

    #[tokio::test]
    async fn truncates_at_50_kib_on_a_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let unit = "é".repeat(64); // 128 bytes; caps land mid-char
        std::fs::write(dir.path().join("wide.txt"), unit.repeat(1000)).unwrap();

        let r = run(dir.path(), serde_json::json!({"path": "wide.txt"})).await;
        assert!(!r.is_error);
        assert!(r.output.contains("truncated"));
        assert!(r.output.len() < MAX_BYTES + 200);
    }

    #[tokio::test]
    async fn missing_file_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let r = run(dir.path(), serde_json::json!({"path": "nope.txt"})).await;
        assert!(r.is_error);
        assert!(r.output.contains("cannot read"));
    }

    #[tokio::test]
    async fn an_endless_file_returns_the_cap_quickly_instead_of_hanging() {
        // /dev/zero never EOFs; the bounded read must return the cap, not
        // read forever.
        let r = run(
            std::path::Path::new("/dev"),
            serde_json::json!({"path": "zero"}),
        )
        .await;
        assert!(!r.is_error);
        assert!(r.output.contains("truncated"));
    }
}
