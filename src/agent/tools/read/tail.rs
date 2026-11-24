//! Tail scanning for a negative `offset`: keep the last N lines while the
//! file streams by, bounded by the scan and page caps.

use std::collections::VecDeque;
use std::io::{BufReader, Read};
use std::path::Path;

use super::super::{Cancel, ToolResult};
use super::{MAX_BYTES, MAX_LINE_CHARS, MAX_SCAN_BYTES};

/// Incremental UTF-8 validator and line-prefix accumulator: bounds per-line
/// memory to `MAX_LINE_CHARS + 1` chars while validating every byte.
struct LineStream {
    carry: [u8; 4],
    carry_len: usize,
    line: String,
    line_chars: usize,
    cr_pending: bool,
}

impl LineStream {
    fn new() -> Self {
        Self {
            carry: [0; 4],
            carry_len: 0,
            line: String::new(),
            line_chars: 0,
            cr_pending: false,
        }
    }

    fn push_char(&mut self, c: char) {
        if self.line_chars <= MAX_LINE_CHARS {
            self.line.push(c);
            self.line_chars += 1;
        }
    }

    fn finish_line(&mut self) -> String {
        self.cr_pending = false;
        self.line_chars = 0;
        let raw = std::mem::take(&mut self.line);
        if raw.chars().count() <= MAX_LINE_CHARS {
            raw
        } else {
            let mut cut: String = raw.chars().take(MAX_LINE_CHARS).collect();
            cut.push('…');
            cut
        }
    }

    fn feed_text(&mut self, s: &str, on_line: &mut impl FnMut(String)) {
        for c in s.chars() {
            if c == '\n' {
                self.cr_pending = false;
                on_line(self.finish_line());
                continue;
            }
            if self.cr_pending {
                self.cr_pending = false;
                self.push_char('\r');
            }
            if c == '\r' {
                self.cr_pending = true;
            } else {
                self.push_char(c);
            }
        }
    }

    fn feed_bytes(
        &mut self,
        bytes: &[u8],
        path: &Path,
        on_line: &mut impl FnMut(String),
    ) -> Result<(), ToolResult> {
        let mut buf;
        let slice = if self.carry_len > 0 {
            buf = Vec::with_capacity(self.carry_len + bytes.len());
            buf.extend_from_slice(&self.carry[..self.carry_len]);
            buf.extend_from_slice(bytes);
            self.carry_len = 0;
            buf.as_slice()
        } else {
            bytes
        };
        match std::str::from_utf8(slice) {
            Ok(s) => {
                self.feed_text(s, on_line);
                Ok(())
            }
            Err(e) if e.error_len().is_none() => {
                let valid = &slice[..e.valid_up_to()];
                if let Ok(s) = std::str::from_utf8(valid) {
                    self.feed_text(s, on_line);
                }
                let rem = &slice[e.valid_up_to()..];
                if rem.len() > 3 {
                    return Err(ToolResult::err(format!(
                        "cannot read {}: not valid UTF-8",
                        path.display()
                    )));
                }
                self.carry[..rem.len()].copy_from_slice(rem);
                self.carry_len = rem.len();
                Ok(())
            }
            Err(_) => Err(ToolResult::err(format!(
                "cannot read {}: not valid UTF-8",
                path.display()
            ))),
        }
    }

    fn finish_eof(
        mut self,
        path: &Path,
        on_line: &mut impl FnMut(String),
    ) -> Result<(), ToolResult> {
        if self.carry_len > 0 {
            return Err(ToolResult::err(format!(
                "cannot read {}: not valid UTF-8",
                path.display()
            )));
        }
        if !self.line.is_empty() {
            on_line(self.finish_line());
        }
        Ok(())
    }
}

pub(super) fn read_tail(path: &Path, want: usize, cancel: &Cancel) -> ToolResult {
    let want = want.max(1);
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
    };
    let mut reader = BufReader::new(file);
    let mut chunk = [0u8; 8192];
    let mut tail: VecDeque<(usize, String)> = VecDeque::new();
    let mut line_no = 0usize;
    let mut scanned = 0usize;
    let mut scan_capped = false;
    let mut stream = LineStream::new();

    loop {
        if cancel.is_cancelled() {
            return ToolResult::err("aborted by user".into());
        }
        let n = match reader.read(&mut chunk) {
            Ok(n) => n,
            Err(e) => return ToolResult::err(format!("cannot read {}: {e}", path.display())),
        };
        if n == 0 {
            if let Err(e) = stream.finish_eof(path, &mut |text| {
                line_no += 1;
                push_tail(&mut tail, line_no, text, want);
            }) {
                return e;
            }
            break;
        }
        scanned += n;
        if scanned > MAX_SCAN_BYTES {
            scan_capped = true;
            break;
        }
        if let Err(e) = stream.feed_bytes(&chunk[..n], path, &mut |text| {
            line_no += 1;
            push_tail(&mut tail, line_no, text, want);
        }) {
            return e;
        }
    }

    if scan_capped {
        return ToolResult::err(format!(
            "scan cap {MAX_SCAN_BYTES} bytes reached before end of file"
        ));
    }

    ToolResult::ok(render_tail(&tail))
}

fn push_tail(tail: &mut VecDeque<(usize, String)>, line_no: usize, text: String, want: usize) {
    if tail.len() == want {
        tail.pop_front();
    }
    tail.push_back((line_no, text));
}

fn render_tail(tail: &VecDeque<(usize, String)>) -> String {
    if tail.is_empty() {
        return String::new();
    }
    let formatted: Vec<(usize, String)> = tail
        .iter()
        .map(|(no, text)| (*no, format!("{no}| {text}")))
        .collect();

    let mut total_bytes = 0usize;
    let mut keep_from = formatted.len();
    for (idx, (_, line)) in formatted.iter().enumerate().rev() {
        let needed = line.len() + usize::from(idx + 1 < formatted.len());
        if total_bytes + needed > MAX_BYTES {
            break;
        }
        total_bytes += needed;
        keep_from = idx;
    }

    let dropped_lines = keep_from > 0;
    if keep_from == formatted.len() {
        // Even the single final line exceeds MAX_BYTES; emit it truncated.
        let (last_no, last_line) = &formatted[formatted.len() - 1];
        let cut = super::floor_char_boundary(last_line, MAX_BYTES);
        let mut out = last_line[..cut].to_string();
        out.push_str(&format!(
            "\n[truncated: byte cap {MAX_BYTES} reached; showing last 1 line ({last_no}-{last_no})]"
        ));
        return out;
    }

    let kept = &formatted[keep_from..];
    let mut out = kept
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if dropped_lines {
        let first = kept[0].0;
        let last = kept[kept.len() - 1].0;
        let noun = if kept.len() == 1 { "line" } else { "lines" };
        out.push_str(&format!(
            "\n[truncated: byte cap {MAX_BYTES} reached; showing last {} {noun} ({first}-{last})]",
            kept.len()
        ));
    }
    out
}
