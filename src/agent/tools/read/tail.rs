//! Tail scanning for a negative `offset`: keep the last N lines while the
//! file streams by, bounded by the scan and page caps.

use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::Path;

use super::super::{Cancel, ToolResult};
use super::{MAX_BYTES, MAX_SCAN_BYTES, cap_line, floor_char_boundary};

pub(super) fn read_tail(path: &Path, want: usize, cancel: &Cancel) -> ToolResult {
    let want = want.max(1);
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
    };
    let mut reader = BufReader::new(file);
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut skipped_partial = false;
    let mut tail: VecDeque<(usize, String)> = VecDeque::new();
    let mut line_no = 0usize;
    let mut scanned = 0usize;
    let mut scan_capped = false;
    let mut eof = false;

    loop {
        if cancel.is_cancelled() {
            return ToolResult::err("aborted by user".into());
        }
        let n = match reader.read(&mut chunk) {
            Ok(n) => n,
            Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
        };
        if n == 0 {
            eof = true;
        } else {
            pending.extend_from_slice(&chunk[..n]);
            scanned += n;
        }

        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let mut raw: Vec<u8> = pending.drain(..=pos).collect();
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let was_partial = std::mem::take(&mut skipped_partial);
            line_no += 1;
            let text = if was_partial {
                ""
            } else {
                match decode(&raw, path) {
                    Ok(t) => t,
                    Err(e) => return e,
                }
            };
            push_tail(&mut tail, line_no, text, want);
        }

        if eof {
            let mut raw = std::mem::take(&mut pending);
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            if !raw.is_empty() || skipped_partial {
                let text = if skipped_partial {
                    ""
                } else {
                    match decode(&raw, path) {
                        Ok(t) => t,
                        Err(e) => return e,
                    }
                };
                line_no += 1;
                push_tail(&mut tail, line_no, text, want);
            }
            break;
        }

        if pending.len() > MAX_BYTES {
            pending.clear();
            skipped_partial = true;
        }

        if scanned >= MAX_SCAN_BYTES {
            scan_capped = true;
            break;
        }
    }

    if scan_capped {
        return ToolResult::err(format!(
            "scan cap {MAX_SCAN_BYTES} bytes reached before end of file"
        ));
    }

    ToolResult::ok(render_tail(&tail))
}

/// Render the retained tail, trimming from the oldest end when it exceeds
/// the page cap so the actual end of the file is never dropped.
fn render_tail(tail: &VecDeque<(usize, String)>) -> String {
    let Some(last) = tail.back() else {
        return String::new();
    };
    let formatted: Vec<String> = tail
        .iter()
        .map(|(no, text)| format!("{no}| {text}"))
        .collect();

    let mut total = 0usize;
    let mut start = formatted.len();
    for (i, line) in formatted.iter().enumerate().rev() {
        let needed = line.len() + usize::from(i + 1 < formatted.len());
        if total + needed > MAX_BYTES {
            break;
        }
        total += needed;
        start = i;
    }

    // The final line alone does not fit: emit it cut to the byte cap.
    if start == formatted.len() {
        let index = formatted.len() - 1;
        let cut = floor_char_boundary(&formatted[index], MAX_BYTES);
        return format!(
            "{}\n[truncated: byte cap {MAX_BYTES} reached; showing last 1 lines ({}-{})]",
            &formatted[index][..cut],
            last.0,
            last.0
        );
    }

    let mut out = formatted[start..].join("\n");
    if start > 0 {
        let emitted = formatted.len() - start;
        out.push_str(&format!(
            "\n[truncated: byte cap {MAX_BYTES} reached; showing last {emitted} lines ({}-{})]",
            tail[start].0, last.0
        ));
    }
    out
}

fn decode<'a>(raw: &'a [u8], path: &Path) -> Result<&'a str, ToolResult> {
    std::str::from_utf8(raw)
        .map_err(|_| ToolResult::err(format!("cannot read {}: not valid UTF-8", path.display())))
}

fn push_tail(tail: &mut VecDeque<(usize, String)>, line_no: usize, text: &str, want: usize) {
    if tail.len() == want {
        tail.pop_front();
    }
    tail.push_back((line_no, cap_line(text)));
}
