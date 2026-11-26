//! The bounded head and tail of a command's output. The pipe is drained to
//! EOF whatever the command does; only these two windows are retained, so
//! an endless stream costs O(cap) memory, and the rendered result is the
//! whole output when it fits or head, marker and tail when it does not.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// The most one command reports: output within this many bytes is returned
/// whole, and above it the head and tail are kept and the rest drained.
pub(super) const OUTPUT_CAP: usize = 10 * 1024;

/// What a command has printed so far, shared between the task draining
/// its pipe and whoever reports it.
pub(super) type SharedCapture = Arc<Mutex<Capture>>;

/// The bounded head and tail of a command's merged output. The pipe is
/// still drained to EOF; only these two windows are retained, so an
/// endless stream costs O(cap) memory.
pub(super) struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl Capture {
    pub(super) fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8], head_cap: usize, tail_cap: usize) {
        self.total += bytes.len();
        let head_room = head_cap.saturating_sub(self.head.len());
        let take = head_room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        for &b in &bytes[take..] {
            self.tail.push_back(b);
            if self.tail.len() > tail_cap {
                self.tail.pop_front();
            }
        }
    }
}

/// The longest valid UTF-8 prefix of `bytes`, trimming only an
/// incomplete character split at the end and bounding lossy expansion to
/// `max_bytes`.
pub(super) fn prefix_string(bytes: &[u8], max_bytes: usize) -> String {
    let mut bytes = bytes;
    if !bytes.is_empty() {
        let mut start = bytes.len() - 1;
        let floor = start.saturating_sub(3);
        while start > floor && (bytes[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        if let Err(e) = std::str::from_utf8(&bytes[start..])
            && e.valid_up_to() == 0
            && e.error_len().is_none()
        {
            bytes = &bytes[..start];
        }
    }
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max_bytes {
        return s.into_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The longest valid UTF-8 suffix of `bytes`, dropping a leading partial
/// character left by the split and bounding lossy expansion to `max_bytes`.
pub(super) fn suffix_string(bytes: &[u8], max_bytes: usize) -> String {
    let mut skip = 0;
    while skip < bytes.len().min(3) && (bytes[skip] & 0xC0) == 0x80 {
        skip += 1;
    }
    let s = String::from_utf8_lossy(&bytes[skip..]);
    if s.len() <= max_bytes {
        return s.into_owned();
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Render the captured bytes: everything when it fits, otherwise the head,
/// a short marker, and the tail. Both splits are trimmed to char boundaries,
/// so the result stays below `cap + 100` bytes.
pub(super) fn render_capture(capture: &Arc<Mutex<Capture>>, cap: usize) -> String {
    let Ok(mut capture) = capture.lock() else {
        return String::new();
    };
    if capture.total <= cap {
        let mut all = capture.head.clone();
        all.extend(capture.tail.iter().copied());
        return String::from_utf8_lossy(&all).into_owned();
    }
    let omitted = capture.total.saturating_sub(cap);
    let head = prefix_string(&capture.head, cap / 4);
    let tail = suffix_string(capture.tail.make_contiguous(), cap - cap / 4);
    format!("{head}\n[... {omitted} bytes omitted ...]\n{tail}")
}
