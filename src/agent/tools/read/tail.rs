//! Tail scanning for a negative `offset`: keep the last N lines while the
//! file streams by, bounded by the scan and page caps.

use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::Path;

use super::super::{Cancel, ToolResult};
use super::scan::ReadOut;
use super::{MAX_BYTES, MAX_SCAN_BYTES, cap_line};

pub(super) fn read_tail(path: &Path, want: usize, cancel: &Cancel) -> ToolResult {
    let want = want.max(1);
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
    };
    let mut reader = BufReader::new(file);
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut tail: VecDeque<(usize, String)> = VecDeque::new();
    let mut line_no = 0usize;
    let mut scanned = 0usize;
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
            line_no += 1;
            let text = match decode(&raw, path) {
                Ok(t) => t,
                Err(e) => return e,
            };
            push_tail(&mut tail, line_no, text, want);
        }

        if eof {
            let mut raw = std::mem::take(&mut pending);
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            if !raw.is_empty() {
                line_no += 1;
                let text = match decode(&raw, path) {
                    Ok(t) => t,
                    Err(e) => return e,
                };
                push_tail(&mut tail, line_no, text, want);
            }
            break;
        }

        if scanned >= MAX_SCAN_BYTES {
            break;
        }
    }

    let mut acc = ReadOut {
        out: String::new(),
        emitted_lines: 0,
        max_emit_lines: want,
        byte_capped: false,
        line_capped: false,
    };
    for (no, text) in &tail {
        if !acc.push_line(*no, text) {
            break;
        }
    }
    if acc.byte_capped
        && let Some((last, _)) = tail.back()
    {
        acc.out = format!(
            "{}\n[truncated: byte cap {MAX_BYTES} reached at line {last}]",
            acc.out
        );
    }
    ToolResult::ok(acc.out)
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
