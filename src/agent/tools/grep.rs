//! `grep`: fixed-substring content search across files, with hard
//! result caps.
//!
//! The pattern is a literal substring, not a regex — adding the `regex`
//! crate as a direct dependency was declined (the no-new-deps rule);
//! callers that need alternation, anchors, or case-insensitivity use
//! the `bash` tool. The caps exist so a broad pattern over a big tree
//! cannot eat the context window: at most `max_results` matches
//! (default 50), each line capped, with the stop visible to the model.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, parse_args};

/// Default cap on matching lines returned.
const DEFAULT_MAX_RESULTS: usize = 50;
/// How much of any one file is searched: grep answers "where", not
/// "what", so the first mebibyte is plenty and bounded IO matters more.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// A matching line longer than this is cut at a char boundary — tool
/// output must never become a file-dump backdoor.
const MAX_LINE_CHARS: usize = 250;

pub struct GrepTool {
    cwd: PathBuf,
}

impl GrepTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        self.cwd.join(path)
    }
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: String,
    max_results: Option<usize>,
}

/// One match: where it is, and the (capped) line it is on.
struct Match {
    location: String,
    line: String,
}

/// Running state of one search; the walk threads this through so the
/// cap can stop it anywhere.
struct Search {
    pattern: String,
    cap: usize,
    matches: Vec<Match>,
    files_searched: usize,
    files_skipped_binary: usize,
    /// The user's stop request, checked as the walk descends: a broad
    /// search over a big tree is the other way to outrun an Esc.
    cancel: Cancel,
    /// Set when the walk gave up on the stop request, so the caller can
    /// report it as the error it is rather than a confident near-miss.
    cancelled: bool,
}

impl Search {
    fn capped(&self) -> bool {
        self.matches.len() >= self.cap
    }

    fn search_file(&mut self, path: &Path) {
        // Per-file granularity: one directory can hold more files than a
        // whole tree, so the walk's per-directory check is not enough.
        if self.cancel.is_cancelled() {
            self.cancelled = true;
            return;
        }
        self.files_searched += 1;
        let mut bytes = Vec::new();
        let loaded =
            std::fs::File::open(path).and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut bytes));
        if loaded.is_err() {
            return; // unreadable file: no matches, not a failure
        }
        if bytes.contains(&0) {
            self.files_skipped_binary += 1;
            return;
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            self.files_skipped_binary += 1;
            return;
        };
        let location_root = super::display(path);
        for (i, line) in content.lines().enumerate() {
            if line.contains(&self.pattern) {
                self.matches.push(Match {
                    location: format!("{location_root}:{}", i + 1),
                    line: cap_line(line),
                });
                if self.capped() {
                    return;
                }
            }
        }
    }

    /// Depth-first, children sorted (deterministic output), `.git`
    /// pruned. Symlinks are treated as leaves — following them could
    /// cycle. Returns false once the cap is reached or the user stopped
    /// the run (either unwinds the walk).
    fn walk(&mut self, dir: &Path) -> bool {
        if self.capped() {
            return false;
        }
        if self.cancel.is_cancelled() {
            self.cancelled = true;
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return true; // unreadable subtree: skipped, the search continues
        };
        let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for child in children {
            if self.cancelled {
                return false;
            }
            let is_dir = std::fs::symlink_metadata(&child)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if is_dir {
                if child.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if !self.walk(&child) {
                    return false;
                }
            } else {
                self.search_file(&child);
                if self.capped() {
                    return false;
                }
            }
        }
        true
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

fn grep(root: &Path, pattern: &str, max_results: usize, cancel: &Cancel) -> ToolResult {
    if pattern.is_empty() {
        return ToolResult::err("pattern must not be empty".into());
    }
    if cancel.is_cancelled() {
        return ToolResult::err("search aborted by user before it started".into());
    }
    // The ROOT is followed (metadata, not symlink_metadata): a
    // symlinked directory is a legitimate search target. In-tree
    // children stay leaves — the walk never follows those.
    let meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(e) => return ToolResult::err(format!("cannot search {}: {e}", root.display())),
    };

    let mut search = Search {
        pattern: pattern.to_string(),
        cap: max_results,
        matches: Vec::new(),
        files_searched: 0,
        files_skipped_binary: 0,
        cancel: cancel.clone(),
        cancelled: false,
    };
    if meta.is_dir() {
        search.walk(root);
    } else {
        search.search_file(root);
    }
    if search.cancelled {
        // Partial matches are shown (they are real work already done), but
        // the result is an error: a stopped walk's "no matches" would read
        // as a searched-and-missed answer.
        return ToolResult::err(format!(
            "{}search aborted by user after {} file(s) — results are partial",
            rendered_matches(&search.matches),
            search.files_searched
        ));
    }
    search.matches.truncate(max_results);

    let mut out = rendered_matches(&search.matches);
    if out.is_empty() {
        // The scope rides along: a miss over 10k files must not read like
        // a miss over two.
        out.push_str(&format!(
            "(no matches in {} file(s))\n",
            search.files_searched
        ));
    }
    if search.matches.len() >= max_results {
        // The search stopped early, so "how many more" is unknowable —
        // the notice says exactly that instead of inventing a count.
        out.push_str(&format!(
            "[truncated: stopped after {max_results} matches — more may exist]\n"
        ));
    }
    if search.files_skipped_binary > 0 {
        out.push_str(&format!(
            "({} binary file(s) skipped)\n",
            search.files_skipped_binary
        ));
    }
    ToolResult::ok(out)
}

/// The match lines in `path:line: text` form.
fn rendered_matches(matches: &[Match]) -> String {
    let mut out = String::new();
    for m in matches {
        out.push_str(&m.location);
        out.push_str(": ");
        out.push_str(&m.line);
        out.push('\n');
    }
    out
}

impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents for a fixed substring (not a regex) and get matching lines as \
path:line: text. Searches a file or a directory tree recursively (.git and binary files \
skipped). Returns at most max_results matching lines (default 50); the stop is noted in the \
output. For regex or case-insensitive search, use the bash tool instead."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Substring to search for (literal text, not a regex)"},
                "path": {"type": "string", "description": "File or directory to search, relative to the working directory"},
                "max_results": {"type": "integer", "description": "Maximum number of matching lines to return (default 50)"}
            },
            "required": ["pattern", "path"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: GrepArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            grep(
                &self.resolve(&args.path),
                &args.pattern,
                args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1),
                cancel,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
        run_with(dir, args, &Cancel::none()).await
    }

    /// The same call with the user's stop request in hand.
    async fn run_with(dir: &std::path::Path, args: Value, cancel: &Cancel) -> ToolResult {
        GrepTool::new(dir.to_path_buf())
            .execute(&args, cancel)
            .await
    }

    fn tree(dir: &std::path::Path) {
        std::fs::write(
            dir.join("top.txt"),
            "alpha needle one\nnothing\nneedle two\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("sub/mid.txt"), "needle three\n").unwrap();
        std::fs::write(dir.join("sub/deep/low.txt"), "the needle four\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/config"), "needle in git\n").unwrap();
    }

    #[tokio::test]
    async fn matches_across_a_tree_sorted_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "needle", "path": "."}),
        )
        .await;
        assert!(!r.is_error);
        let root = dir.path().display().to_string();
        assert_eq!(
            r.output,
            format!(
                "{root}/sub/deep/low.txt:1: the needle four\n\
                 {root}/sub/mid.txt:1: needle three\n\
                 {root}/top.txt:1: alpha needle one\n\
                 {root}/top.txt:3: needle two\n"
            ),
            "sorted file order, one line per match, .git pruned"
        );
    }

    #[tokio::test]
    async fn no_match_is_a_clean_not_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "nothing here\n").unwrap();

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "needle", "path": "."}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(r.output, "(no matches in 1 file(s))\n");
    }

    #[tokio::test]
    async fn caps_results_with_a_visible_stop_notice() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..20).map(|i| format!("hit {i}\n")).collect();
        std::fs::write(dir.path().join("many.txt"), body).unwrap();

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "hit", "path": ".", "max_results": 5}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(r.output.match_indices("hit").count(), 5, "5 match lines");
        assert!(r.output.contains("[truncated: stopped after 5 matches"));
        assert!(r.output.contains("hit 4\n"));
        assert!(!r.output.contains("hit 5\n"));
    }

    #[tokio::test]
    async fn binary_files_are_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("text.txt"), "needle in text\n").unwrap();
        std::fs::write(dir.path().join("blob.bin"), b"needle\x00binary").unwrap();

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "needle", "path": "."}),
        )
        .await;
        assert!(!r.is_error);
        assert!(r.output.contains("text.txt"));
        assert!(!r.output.contains("blob.bin:"));
        assert!(r.output.contains("(1 binary file(s) skipped)"));
    }

    #[tokio::test]
    async fn missing_path_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "x", "path": "nope"}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("cannot search"));
    }

    /// A symlinked directory as the search ROOT is followed — the old
    /// symlink_metadata check sent it down the file path and swallowed
    /// the EISDIR as a confident-looking "(no matches)".
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_root_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let link = dir.path().join("link-sub");
        std::os::unix::fs::symlink(dir.path().join("sub"), &link).unwrap();

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "needle three", "path": "link-sub"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.output);
        assert!(r.output.contains("mid.txt:1"), "{}", r.output);
    }

    #[tokio::test]
    async fn a_single_file_path_searches_that_file() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "needle three", "path": "sub/mid.txt"}),
        )
        .await;
        assert!(!r.is_error);
        let root = dir.path().display().to_string();
        assert_eq!(r.output, format!("{root}/sub/mid.txt:1: needle three\n"));
    }

    #[tokio::test]
    async fn overlong_matching_lines_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("x{}y\n", "p".repeat(1000));
        std::fs::write(dir.path().join("long.txt"), long).unwrap();

        let r = run(dir.path(), serde_json::json!({"pattern": "x", "path": "."})).await;
        assert!(!r.is_error);
        assert!(r.output.contains('…'));
        assert!(
            r.output.chars().count() < 500,
            "line capped well below 1000 chars"
        );
    }

    /// Esc before the call: the search never starts, and the answer says
    /// so as an error rather than as a confident miss.
    #[tokio::test]
    async fn a_cancelled_search_reports_aborted_without_searching() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let flag = Arc::new(AtomicBool::new(true));

        let r = run_with(
            dir.path(),
            serde_json::json!({"pattern": "needle", "path": "."}),
            &Cancel::new(flag),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("aborted by user"), "{}", r.output);
    }

    /// Esc mid-walk: a stop request raised while the search is in flight
    /// ends it. The walk is synchronous, so the flag comes from a real
    /// thread, and its delay is a quarter of this machine's measured cost
    /// of the same walk — so the stop lands inside the walk here as surely
    /// as on a faster or slower box. The files-searched count in the
    /// answer is the proof the walk really stopped early.
    #[tokio::test]
    async fn a_cancelled_walk_stops_promptly_and_says_it_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        let (dirs, per_dir) = (400usize, 50usize);
        for i in 0..dirs {
            let sub = dir.path().join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            for j in 0..per_dir {
                std::fs::write(sub.join(format!("f{j}.txt")), "text\n").unwrap();
            }
        }
        let total = dirs * per_dir;
        // No pattern matches, so the only thing the walk reports is scope.
        let args = |max: usize| serde_json::json!({"pattern": "no-such-needle", "path": ".", "max_results": max});

        let measured = std::time::Instant::now();
        let complete = run(dir.path(), args(total + 10)).await;
        let walk_time = measured.elapsed();
        assert!(!complete.is_error);
        assert!(
            complete.output.contains(&format!("{total} file(s)")),
            "the uncancelled walk covers the tree: {}",
            complete.output
        );

        let delay = walk_time / 4;
        let flag = Arc::new(AtomicBool::new(false));
        let setter = {
            let flag = flag.clone();
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                flag.store(true, Ordering::Relaxed);
            })
        };

        let r = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_with(dir.path(), args(total + 10), &Cancel::new(flag)),
        )
        .await
        .expect("a cancelled walk must not run on to the end of the tree");
        setter.join().unwrap();

        assert!(r.is_error, "a stopped search is an error: {}", r.output);
        assert!(r.output.contains("aborted by user"), "{}", r.output);
        assert!(r.output.contains("partial"), "{}", r.output);
        let searched: usize = r
            .output
            .split("after ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .expect("the aborted result names the scope it covered");
        assert!(
            searched > 0 && searched < total,
            "the walk must stop partway, covered {searched} of {total}"
        );
    }
}
