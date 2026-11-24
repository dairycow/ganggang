//! The search itself: one tree walked with a compiled pattern, capped and
//! stop-aware, and the grouped report it gives when the walk is over.

use std::io::Read;
use std::path::Path;

use globset::GlobMatcher;
use regex::Regex;

use super::{
    Cancel, MAX_FILE_BYTES, MAX_LINE_CHARS, MAX_OUTPUT_BYTES, ToolResult, display_relative,
};

pub(super) struct Search<'a> {
    cwd: &'a Path,
    regex: Regex,
    glob: Option<(GlobMatcher, bool)>,
    context: usize,
    cap: usize,
    files_only: bool,
    match_count: usize,
    lines_out: Vec<String>,
    output_bytes: usize,
    byte_capped: bool,
    files_searched: usize,
    files_skipped_binary: usize,
    /// The user's stop request, checked as the walk descends: a broad
    /// search over a big tree is the other way to outrun an Esc. The walk
    /// runs on a blocking thread, so dropping the call's future cannot
    /// stop it — only this can.
    cancel: Cancel,
    /// Set when the walk gave up on the stop request, so the caller can
    /// report it as the error it is rather than a confident near-miss.
    cancelled: bool,
}

impl<'a> Search<'a> {
    /// A search over one tree: the compiled pattern, the optional glob,
    /// the context width, the result cap, the file-only mode, and the
    /// run's stop request.
    pub(super) fn new(
        cwd: &'a Path,
        regex: regex::Regex,
        glob: Option<(globset::GlobMatcher, bool)>,
        context: usize,
        cap: usize,
        files_only: bool,
        cancel: &Cancel,
    ) -> Self {
        Self {
            cwd,
            regex,
            glob,
            context,
            cap,
            files_only,
            match_count: 0,
            lines_out: Vec::new(),
            output_bytes: 0,
            byte_capped: false,
            files_searched: 0,
            files_skipped_binary: 0,
            cancel: cancel.clone(),
            cancelled: false,
        }
    }

    pub(super) fn capped(&self) -> bool {
        self.match_count >= self.cap || self.byte_capped
    }

    #[cfg(test)]
    /// Force the state a walk reaches when its first output line did not
    /// fit, so `report` can be tested without a 16 KiB line.
    pub(super) fn force_byte_capped(&mut self) {
        self.byte_capped = true;
    }

    pub(super) fn stopped(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.cancelled = true;
            true
        } else {
            false
        }
    }

    /// Add one output line unless it would push the report past the hard
    /// byte cap.
    fn push_line(&mut self, line: String) -> bool {
        let needed = line.len() + usize::from(!self.lines_out.is_empty());
        if self.output_bytes + needed > MAX_OUTPUT_BYTES {
            self.byte_capped = true;
            return false;
        }
        self.output_bytes += needed;
        self.lines_out.push(line);
        true
    }

    /// The walk is over: what it found, grouped by file. A walk the user
    /// stopped answers with what it had, as an error — its "no matches"
    /// would read as a searched-and-missed answer.
    pub(super) fn report(self) -> ToolResult {
        let mut out = self.lines_out.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        if self.cancelled {
            return ToolResult::err(format!(
                "{out}search aborted by user after {} file(s) — results are partial\n",
                self.files_searched
            ));
        }
        if out.is_empty() && !self.byte_capped {
            out.push_str(&format!(
                "(no matches in {} file(s))\n",
                self.files_searched
            ));
        }
        if self.byte_capped {
            out.push_str(&format!(
                "[truncated: output capped at {MAX_OUTPUT_BYTES} bytes — narrow pattern or path]\n"
            ));
        }
        if self.match_count >= self.cap {
            out.push_str(&format!(
                "[truncated: stopped after {} matches — more may exist]\n",
                self.cap
            ));
        }
        if self.files_skipped_binary > 0 {
            out.push_str(&format!(
                "({} binary file(s) skipped)\n",
                self.files_skipped_binary
            ));
        }
        ToolResult::ok(out)
    }

    fn matches_glob(&self, display_path: &Path) -> bool {
        let Some((matcher, match_rel)) = &self.glob else {
            return true;
        };
        if *match_rel {
            let rel = display_relative(self.cwd, display_path);
            matcher.is_match(rel)
        } else {
            display_path
                .file_name()
                .is_some_and(|n| matcher.is_match(n))
        }
    }

    pub(super) fn search_file(&mut self, disk_path: &Path, display_path: &Path) {
        // Per-file granularity: one directory can hold more files than a
        // whole tree, so the walk's per-entry check is not enough.
        if self.stopped() || !self.matches_glob(display_path) {
            return;
        }
        self.files_searched += 1;
        let mut bytes = Vec::new();
        let loaded = std::fs::File::open(disk_path)
            .and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut bytes));
        if loaded.is_err() {
            return;
        }
        if bytes.contains(&0) {
            self.files_skipped_binary += 1;
            return;
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            self.files_skipped_binary += 1;
            return;
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let remaining_cap = self.cap.saturating_sub(self.match_count);
        let mut match_indices = Vec::new();
        for (i, line) in all_lines.iter().enumerate() {
            if self.regex.is_match(line) {
                match_indices.push(i);
                if match_indices.len() >= remaining_cap {
                    break;
                }
            }
        }
        if match_indices.is_empty() {
            return;
        }
        self.match_count += match_indices.len();

        let loc = display_relative(self.cwd, display_path);
        let entries = if self.files_only {
            vec![format!("{loc} ({})", match_indices.len())]
        } else if self.context == 0 {
            let mut entries = vec![format!("{loc}:")];
            for &i in &match_indices {
                entries.push(format!("  {}: {}", i + 1, cap_line(all_lines[i])));
            }
            entries
        } else {
            self.context_entries(&loc, &all_lines, &match_indices)
        };

        // A header that fits but whose first line does not would leave a
        // dangling file name; drop it again.
        let before_len = self.lines_out.len();
        let before_bytes = self.output_bytes;
        let header_only = entries.len() > 1;
        for entry in entries {
            if !self.push_line(entry) {
                break;
            }
        }
        if header_only && self.lines_out.len() == before_len + 1 {
            self.lines_out.truncate(before_len);
            self.output_bytes = before_bytes;
        }
    }

    fn context_entries(
        &self,
        loc: &str,
        all_lines: &[&str],
        match_indices: &[usize],
    ) -> Vec<String> {
        let last_idx = all_lines.len().saturating_sub(1);
        let mut intervals: Vec<(usize, usize)> = Vec::new();
        for &i in match_indices {
            let start = i.saturating_sub(self.context);
            let end = (i + self.context).min(last_idx);
            if let Some(prev) = intervals.last_mut()
                && start <= prev.1 + 1
            {
                prev.1 = prev.1.max(end);
            } else {
                intervals.push((start, end));
            }
        }
        let mut entries = vec![format!("{loc}:")];
        for (idx, (start, end)) in intervals.into_iter().enumerate() {
            if idx > 0 {
                entries.push("  --".to_string());
            }
            for (j, ctx_line) in all_lines.iter().enumerate().take(end + 1).skip(start) {
                let sep = if match_indices.binary_search(&j).is_ok() {
                    ':'
                } else {
                    '-'
                };
                entries.push(format!("  {}{sep} {}", j + 1, cap_line(ctx_line)));
            }
        }
        entries
    }
}

fn cap_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        line.to_string()
    } else {
        let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
        format!("{cut}…")
    }
}
