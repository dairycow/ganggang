//! `grep`: regex/literal content search across files, with `.gitignore`
//! awareness and hard result caps.

use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value;

use super::{
    BoxFuture, Cancel, PRUNED_DIRS, Tool, ToolResult, display_relative, parse_args,
    resolve_walk_root,
};

const DEFAULT_MAX_RESULTS: usize = 50;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LINE_CHARS: usize = 250;
const MAX_CONTEXT_LINES: usize = 10;

pub struct GrepTool {
    cwd: PathBuf,
}

impl GrepTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: Option<&str>) -> PathBuf {
        self.cwd.join(path.unwrap_or("."))
    }
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    case_insensitive: Option<bool>,
    fixed_strings: Option<bool>,
    context: Option<usize>,
    max_results: Option<usize>,
}

struct Search<'a> {
    cwd: &'a Path,
    regex: Regex,
    glob: Option<(GlobMatcher, bool)>,
    context: usize,
    cap: usize,
    match_count: usize,
    lines_out: Vec<String>,
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
    fn capped(&self) -> bool {
        self.match_count >= self.cap
    }

    fn stopped(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.cancelled = true;
            true
        } else {
            false
        }
    }

    /// The walk is over: what it found. A walk the user stopped answers
    /// with what it had, as an error — its "no matches" would read as a
    /// searched-and-missed answer.
    fn report(self, cap: usize) -> ToolResult {
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
        if out.is_empty() {
            out.push_str(&format!(
                "(no matches in {} file(s))\n",
                self.files_searched
            ));
        }
        if self.match_count >= cap {
            out.push_str(&format!(
                "[truncated: stopped after {cap} matches — more may exist]\n"
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

    fn search_file(&mut self, disk_path: &Path, display_path: &Path) {
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
        if self.context == 0 {
            for &i in &match_indices {
                self.lines_out
                    .push(format!("{loc}:{}: {}", i + 1, cap_line(all_lines[i])));
            }
            return;
        }

        let last_idx = all_lines.len().saturating_sub(1);
        let mut intervals: Vec<(usize, usize)> = Vec::new();
        for &i in &match_indices {
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
        for (idx, (start, end)) in intervals.into_iter().enumerate() {
            if idx > 0 {
                self.lines_out.push("--".to_string());
            }
            for (j, ctx_line) in all_lines.iter().enumerate().take(end + 1).skip(start) {
                let sep = if match_indices.binary_search(&j).is_ok() {
                    ':'
                } else {
                    '-'
                };
                self.lines_out
                    .push(format!("{loc}{sep}{}{sep} {}", j + 1, cap_line(ctx_line)));
            }
        }
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

fn grep(cwd: &Path, root: &Path, args: &GrepArgs, cancel: &Cancel) -> ToolResult {
    if args.pattern.is_empty() {
        return ToolResult::err("pattern must not be empty".into());
    }
    let pat = if args.fixed_strings.unwrap_or(false) {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    let regex = match RegexBuilder::new(&pat)
        .case_insensitive(args.case_insensitive.unwrap_or(false))
        .build()
    {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("invalid regex: {e}")),
    };
    let glob = match &args.glob {
        Some(g) => match GlobBuilder::new(g).build() {
            Ok(compiled) => Some((compiled.compile_matcher(), g.contains('/'))),
            Err(e) => return ToolResult::err(format!("invalid glob: {e}")),
        },
        None => None,
    };

    let meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(e) => return ToolResult::err(format!("cannot search {}: {e}", root.display())),
    };

    let cap = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
    let context = args.context.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let mut search = Search {
        cwd,
        regex,
        glob,
        context,
        cap,
        match_count: 0,
        lines_out: Vec::new(),
        files_searched: 0,
        files_skipped_binary: 0,
        cancel: cancel.clone(),
        cancelled: false,
    };

    if meta.is_file() {
        search.search_file(root, root);
    } else {
        let (walk_root, was_symlink) = resolve_walk_root(root);
        let walker = WalkBuilder::new(&walk_root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            .require_git(false)
            .follow_links(false)
            .sort_by_file_path(|a, b| a.cmp(b))
            .filter_entry(|e| {
                e.depth() == 0
                    || !(e.file_type().is_some_and(|ft| ft.is_dir())
                        && PRUNED_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))
            })
            .build();

        for entry in walker.flatten() {
            if search.stopped() {
                break;
            }
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let disk_path = entry.path();
            let display_path = if was_symlink {
                disk_path
                    .strip_prefix(&walk_root)
                    .map(|rel| root.join(rel))
                    .unwrap_or_else(|_| disk_path.to_path_buf())
            } else {
                disk_path.to_path_buf()
            };
            search.search_file(disk_path, &display_path);
            if search.capped() {
                break;
            }
        }
    }
    search.report(cap)
}

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents using a regular expression (or literal text with fixed_strings: true). \
Respects .gitignore and skips .git, target, node_modules, and binary files. Supports glob \
filtering, case_insensitive, and context lines. Returns at most max_results matches (default 50)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression to search for (or literal text when fixed_strings is true)"},
                "path": {"type": "string", "description": "File or directory to search, relative to the working directory (default: \".\")"},
                "glob": {"type": "string", "description": "Optional glob filter on file names or paths, e.g. \"*.rs\""},
                "case_insensitive": {"type": "boolean", "description": "Case-insensitive matching (default: false)"},
                "fixed_strings": {"type": "boolean", "description": "Treat pattern as literal text instead of regex (default: false)"},
                "context": {"type": "integer", "description": "Number of context lines before and after each match (max 10)"},
                "max_results": {"type": "integer", "description": "Maximum number of matching lines to return (default 50)"}
            },
            "required": ["pattern"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return ToolResult::err("search aborted by user before it started".into());
            }
            let args: GrepArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let cwd = self.cwd.clone();
            let root = self.resolve(args.path.as_deref());
            let cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || grep(&cwd, &root, &args, &cancel)).await {
                Ok(res) => res,
                Err(e) => ToolResult::err(format!("grep task failed: {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests;
