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
    let mut scanned_bytes = 0usize;
    let mut line_no = 0usize;
    let mut out = String::new();
    let mut emitted_lines = 0usize;
    let mut byte_capped = false;
    let mut line_capped = false;
    let mut scan_capped = false;
    let mut eof = false;

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
            let text = match std::str::from_utf8(&raw) {
                Ok(s) => s,
                Err(_) => {
                    return ToolResult::err(format!(
                        "cannot read {}: not valid UTF-8",
                        path.display()
                    ));
                }
            };
            line_no += 1;
            if line_no < start_line {
                continue;
            }
            if emitted_lines >= max_emit_lines {
                line_capped = true;
                break 'outer;
            }
            let formatted = format!("{line_no}| {text}");
            let needed = formatted.len() + usize::from(!out.is_empty());
            if out.len() + needed > MAX_BYTES {
                if out.is_empty() {
                    let prefix = format!("{line_no}| ");
                    let room = MAX_BYTES.saturating_sub(prefix.len());
                    let cut = floor_char_boundary(text, room);
                    out.push_str(&prefix);
                    out.push_str(&text[..cut]);
                    emitted_lines = 1;
                }
                byte_capped = true;
                break 'outer;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&formatted);
            emitted_lines += 1;
        }

        if eof {
            if !pending.is_empty() {
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
                line_no += 1;
                if line_no >= start_line {
                    if emitted_lines >= max_emit_lines {
                        line_capped = true;
                    } else {
                        let formatted = format!("{line_no}| {text}");
                        let needed = formatted.len() + usize::from(!out.is_empty());
                        if out.len() + needed > MAX_BYTES {
                            if out.is_empty() {
                                let prefix = format!("{line_no}| ");
                                let room = MAX_BYTES.saturating_sub(prefix.len());
                                let cut = floor_char_boundary(&text, room);
                                out.push_str(&prefix);
                                out.push_str(&text[..cut]);
                                emitted_lines = 1;
                            }
                            byte_capped = true;
                        } else {
                            if !out.is_empty() {
                                out.push('\n');
                            }
                            out.push_str(&formatted);
                            emitted_lines += 1;
                        }
                    }
                }
            }
            break;
        }

        if line_no + 1 >= start_line && pending.len() > MAX_BYTES {
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
            if out.is_empty() {
                let cur_line = line_no + 1;
                let prefix = format!("{cur_line}| ");
                let room = MAX_BYTES.saturating_sub(prefix.len());
                let cut = floor_char_boundary(&text, room);
                out.push_str(&prefix);
                out.push_str(&text[..cut]);
                emitted_lines = 1;
            }
            byte_capped = true;
            break;
        }

        if scanned_bytes >= MAX_SCAN_BYTES {
            scan_capped = true;
            break;
        }
    }

    if emitted_lines == 0 && !byte_capped && !line_capped && !scan_capped && line_no > 0 {
        return ToolResult::ok(format!(
            "[offset {start_line} is past end of file ({line_no} lines)]"
        ));
    }

    let hit_cap = byte_capped
        || (line_capped && (limit.is_none() || max_emit_lines == MAX_LINES))
        || scan_capped;
    if hit_cap {
        let last_line = start_line + emitted_lines.saturating_sub(1);
        let mut notes = Vec::new();
        if line_capped {
            let total_hint = if file_len > 0 && file_len <= 1024 * 1024 {
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
        if byte_capped {
            notes.push(format!("byte cap {MAX_BYTES} reached at line {last_line}"));
        }
        if scan_capped {
            notes.push(format!("scan cap {MAX_SCAN_BYTES} bytes reached"));
        }
        notes.push(format!("pass offset={} to continue", last_line + 1));
        out = format!("{out}\n[truncated: {}]", notes.join("; "));
    }

    ToolResult::ok(out)
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
