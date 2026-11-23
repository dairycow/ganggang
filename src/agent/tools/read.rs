//! `read`: text file → numbered lines, with hard output caps.
//!
//! Streams sequentially so `offset` pages cleanly past 50 KiB without
//! loading or truncating the whole file first, while a scan cap keeps
//! endless files like `/dev/zero` bounded.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Tool, ToolResult, parse_args};

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 50 * 1024;
const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;

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

struct ReadOut {
    out: String,
    emitted_lines: usize,
    max_emit_lines: usize,
    byte_capped: bool,
    line_capped: bool,
}

impl ReadOut {
    fn push_line(&mut self, line_no: usize, text: &str) -> bool {
        if self.emitted_lines >= self.max_emit_lines {
            self.line_capped = true;
            return false;
        }
        let formatted = format!("{line_no}| {text}");
        let needed = formatted.len() + usize::from(!self.out.is_empty());
        if self.out.len() + needed > MAX_BYTES {
            if self.out.is_empty() {
                let prefix = format!("{line_no}| ");
                let room = MAX_BYTES.saturating_sub(prefix.len());
                let cut = floor_char_boundary(text, room);
                self.out.push_str(&prefix);
                self.out.push_str(&text[..cut]);
                self.emitted_lines = 1;
            }
            self.byte_capped = true;
            return false;
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out.push_str(&formatted);
        self.emitted_lines += 1;
        true
    }
}

fn read(path: &Path, offset: Option<usize>, limit: Option<usize>) -> ToolResult {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start_line = offset.unwrap_or(1).max(1);
    let max_emit_lines = limit.unwrap_or(MAX_LINES).min(MAX_LINES);

    let mut reader = BufReader::new(file);
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut skipped_partial = false;
    let mut scanned_bytes = 0usize;
    let mut line_no = 0usize;
    let mut scan_capped = false;
    let mut eof = false;
    let mut acc = ReadOut {
        out: String::new(),
        emitted_lines: 0,
        max_emit_lines,
        byte_capped: false,
        line_capped: false,
    };

    'outer: loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) => {
                eof = true;
                0
            }
            Ok(n) => n,
            Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
        };
        if n > 0 {
            pending.extend_from_slice(&chunk[..n]);
            scanned_bytes += n;
        }

        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let mut raw: Vec<u8> = pending.drain(..=pos).collect();
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let was_partial = std::mem::take(&mut skipped_partial);
            line_no += 1;
            if line_no < start_line {
                continue;
            }
            let text = if was_partial {
                ""
            } else {
                match std::str::from_utf8(&raw) {
                    Ok(s) => s,
                    Err(_) => {
                        return ToolResult::err(format!(
                            "cannot read {}: not valid UTF-8",
                            path.display()
                        ));
                    }
                }
            };
            if !acc.push_line(line_no, text) {
                break 'outer;
            }
        }

        if eof {
            let mut tail = std::mem::take(&mut pending);
            if tail.last() == Some(&b'\r') {
                tail.pop();
            }
            if !tail.is_empty() || skipped_partial {
                let text = if skipped_partial {
                    ""
                } else {
                    match std::str::from_utf8(&tail) {
                        Ok(s) => s,
                        Err(_) => {
                            return ToolResult::err(format!(
                                "cannot read {}: not valid UTF-8",
                                path.display()
                            ));
                        }
                    }
                };
                line_no += 1;
                if line_no >= start_line {
                    acc.push_line(line_no, text);
                }
            }
            break;
        }

        if line_no + 1 < start_line {
            if pending.len() > MAX_BYTES {
                pending.clear();
                skipped_partial = true;
            }
        } else if pending.len() > MAX_BYTES {
            let text = match std::str::from_utf8(&pending) {
                Ok(s) => s.to_string(),
                Err(e) if e.error_len().is_none() => {
                    String::from_utf8_lossy(&pending[..e.valid_up_to()]).into_owned()
                }
                Err(_) => {
                    return ToolResult::err(format!(
                        "cannot read {}: not valid UTF-8",
                        path.display()
                    ));
                }
            };
            acc.push_line(line_no + 1, &text);
            acc.byte_capped = true;
            break;
        }

        if scanned_bytes >= MAX_SCAN_BYTES {
            scan_capped = true;
            break;
        }
    }

    if scan_capped && acc.emitted_lines == 0 {
        return ToolResult::err(format!(
            "scan cap {MAX_SCAN_BYTES} bytes reached before line {start_line}"
        ));
    }

    if acc.emitted_lines == 0 && !acc.byte_capped && !acc.line_capped && !scan_capped && line_no > 0
    {
        return ToolResult::ok(format!(
            "[offset {start_line} is past end of file ({line_no} lines)]"
        ));
    }

    let hit_cap = acc.byte_capped
        || (acc.line_capped && (limit.is_none() || max_emit_lines == MAX_LINES))
        || scan_capped;
    if hit_cap {
        let last_line = start_line + acc.emitted_lines.saturating_sub(1);
        let mut notes = Vec::new();
        if acc.line_capped {
            let total_hint = if eof {
                format!(" of {line_no}")
            } else if file_len > 0 && file_len <= 1024 * 1024 {
                count_remaining_lines(&pending, &mut reader, line_no)
                    .map(|t| format!(" of {t}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            notes.push(format!(
                "showing lines {start_line}-{last_line}{total_hint}"
            ));
        }
        if acc.byte_capped {
            notes.push(format!("byte cap {MAX_BYTES} reached at line {last_line}"));
        }
        if scan_capped {
            notes.push(format!("scan cap {MAX_SCAN_BYTES} bytes reached"));
        }
        notes.push(format!("pass offset={} to continue", last_line + 1));
        acc.out = format!("{}\n[truncated: {}]", acc.out, notes.join("; "));
    }

    ToolResult::ok(acc.out)
}

fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_bytes)
        .last()
        .unwrap_or(0)
}

fn count_remaining_lines<R: Read>(
    pending: &[u8],
    reader: &mut R,
    mut count: usize,
) -> Option<usize> {
    let mut saw_trailing_non_nl = false;
    for &b in pending {
        if b == b'\n' {
            count += 1;
            saw_trailing_non_nl = false;
        } else {
            saw_trailing_non_nl = true;
        }
    }
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).ok()?;
        if n == 0 {
            if saw_trailing_non_nl {
                count += 1;
            }
            return Some(count);
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                count += 1;
                saw_trailing_non_nl = false;
            } else {
                saw_trailing_non_nl = true;
            }
        }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read a text file with 1-based line numbers (N| text). Returns up to 2000 lines / \
50 KiB; truncated reads say so. Use offset (1-based line) and limit to page through \
larger files (including past 50 KiB)."
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
mod tests;
