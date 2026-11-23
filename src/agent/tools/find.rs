//! `find`: locate entries by name glob, with a visible result cap.
//!
//! Globs are `*` and `?` on the entry NAME (no character classes, no
//! `**` — a dependency-free matcher that behaves predictably beats a
//! half-compatible glob dialect). The walk is sorted and deterministic,
//! hidden entries included; `max_results` (default 50) bounds the
//! output and the stop is noted.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, display, parse_args};

const DEFAULT_MAX_RESULTS: usize = 50;

pub struct FindTool {
    cwd: PathBuf,
}

impl FindTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: Option<&str>) -> PathBuf {
        self.cwd.join(path.unwrap_or("."))
    }
}

#[derive(Deserialize)]
struct FindArgs {
    pattern: String,
    path: Option<String>,
    max_results: Option<usize>,
}

/// Match a name against a `*`/`?` glob. `*` spans any run of chars
/// within the name (names never contain `/`, so no segment semantics).
fn glob(pattern: &[char], name: &[char]) -> bool {
    // Iterative two-pointer match that backtracks only to the last `*`:
    // O(len(name)·len(pattern)) worst case. The obvious recursive
    // formulation is exponential on star-heavy patterns (a model-emitted
    // `*a*a*a*a*b` against a long name would wedge the run — there is
    // no per-tool timeout and abort lands between calls).
    let (mut p, mut n) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = p;
            mark = n;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn find(root: &Path, pattern: &str, max_results: usize, cancel: &Cancel) -> ToolResult {
    if pattern.is_empty() {
        return ToolResult::err("pattern must not be empty".into());
    }
    if cancel.is_cancelled() {
        return ToolResult::err("search aborted by user before it started".into());
    }
    let pat: Vec<char> = pattern.chars().collect();
    // The ROOT is followed (metadata, not symlink_metadata): a
    // symlinked directory is a legitimate search target. In-tree
    // children stay leaves — the walk never follows those.
    let Ok(meta) = std::fs::metadata(root) else {
        return ToolResult::err(format!("cannot search {}: not found", display(root)));
    };
    if !meta.is_dir() {
        return ToolResult::err(format!("cannot search {}: not a directory", display(root)));
    }

    let mut hits = Vec::new();
    // `stopped` distinguishes a walk that ran out of tree from one the
    // user stopped: a stopped walk's "(no matches)" would be a lie.
    let stopped = walk(root, &pat, &mut hits, max_results, cancel);
    if stopped.is_stopped() {
        // Partial hits ride along (they are real work already done), but
        // the result is an error so the model cannot read a truncated
        // walk as the complete answer.
        let mut out = String::new();
        for hit in &hits {
            out.push_str(hit);
            out.push('\n');
        }
        out.push_str(if hits.is_empty() {
            "search aborted by user before any match"
        } else {
            "search aborted by user — results are partial"
        });
        out.push('\n');
        return ToolResult::err(out);
    }

    let mut out = String::new();
    for hit in &hits {
        out.push_str(hit);
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("(no matches)\n");
    }
    if hits.len() >= max_results {
        out.push_str(&format!(
            "[truncated: stopped after {max_results} matches — more may exist]\n"
        ));
    }
    ToolResult::ok(out)
}

/// Depth-first, children sorted, every entry considered (hidden
/// included — finding a hidden file is a legitimate query). Symlinks
/// are leaves: following them could cycle. The walk stops at the cap or
/// on the user's stop request, and reports which (unwinding the
/// recursion either way).
#[derive(PartialEq, Eq)]
enum Walk {
    /// Every reachable entry was considered, or the cap was reached.
    Done,
    /// The user stopped the run partway through the tree.
    Stopped,
}

impl Walk {
    fn is_stopped(&self) -> bool {
        *self == Self::Stopped
    }
}

fn walk(dir: &Path, pat: &[char], hits: &mut Vec<String>, cap: usize, cancel: &Cancel) -> Walk {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Walk::Done; // unreadable subtree: skipped, the search continues
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    children.sort();
    for child in children {
        // One relaxed load per entry: enough resolution to stop a walk
        // through a huge tree promptly without measuring the tree first.
        if cancel.is_cancelled() {
            return Walk::Stopped;
        }
        let name: Vec<char> = child
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned())
            .chars()
            .collect();
        if glob(pat, &name) {
            hits.push(display(&child));
            if hits.len() >= cap {
                return Walk::Done;
            }
        }
        let is_dir = std::fs::symlink_metadata(&child)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir && walk(&child, pat, hits, cap, cancel).is_stopped() {
            return Walk::Stopped;
        }
    }
    Walk::Done
}

impl Tool for FindTool {
    fn name(&self) -> &'static str {
        "find"
    }

    fn description(&self) -> &'static str {
        "Find files and directories whose NAME matches a glob (* = any run of characters, \
? = one character; no character classes). Searches recursively from a root directory \
(default the working directory), dotfiles included, results sorted. Returns at most \
max_results paths (default 50); the stop is noted in the output."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob matched against entry names, e.g. \"*.rs\" or \"config?\""},
                "path": {"type": "string", "description": "Directory to search from (default: the working directory)"},
                "max_results": {"type": "integer", "description": "Maximum number of paths to return (default 50)"}
            },
            "required": ["pattern"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: FindArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            find(
                &self.resolve(args.path.as_deref()),
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

    async fn run(dir: &Path, args: Value) -> ToolResult {
        run_with(dir, args, &Cancel::none()).await
    }

    /// The same call with the user's stop request in hand.
    async fn run_with(dir: &Path, args: Value, cancel: &Cancel) -> ToolResult {
        FindTool::new(dir.to_path_buf())
            .execute(&args, cancel)
            .await
    }

    fn tree(dir: &Path) {
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("src/deep/util.rs"), "").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".hidden/secret.rs"), "").unwrap();
    }

    #[tokio::test]
    async fn matches_names_by_glob_sorted_and_hidden_included() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({"pattern": "*.rs"})).await;
        assert!(!r.is_error);
        let root = dir.path().display().to_string();
        assert_eq!(
            r.output,
            format!("{root}/.hidden/secret.rs\n{root}/src/deep/util.rs\n{root}/src/main.rs\n")
        );
    }

    /// The matcher must stay linear-ish on star-heavy patterns: the
    /// recursive formulation wedged for minutes on exactly this shape
    /// (final review bug #1). Correctness included, since a fast wrong
    /// answer would be worse.
    #[test]
    fn pathological_star_patterns_terminate_and_stay_correct() {
        let name: Vec<char> = "a".repeat(300).chars().collect();
        let wedge: Vec<char> = "*a*a*a*a*a*a*a*a*b".chars().collect();
        assert!(!glob(&wedge, &name));
        assert!(glob(
            &wedge,
            &format!("{}b", "a".repeat(300)).chars().collect::<Vec<_>>()
        ));
        let stars: Vec<char> = "*".repeat(64).chars().collect();
        assert!(glob(&stars, &name));
        let no_star: Vec<char> = "a?c".chars().collect();
        assert!(glob(&no_star, &"abc".chars().collect::<Vec<_>>()));
        assert!(!glob(&no_star, &"abbc".chars().collect::<Vec<_>>()));
    }

    /// A symlinked directory as the search ROOT is followed (a legitimate
    /// target); symlinks found IN the tree stay leaves.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_root_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let link = dir.path().join("link-src");
        std::os::unix::fs::symlink(dir.path().join("src"), &link).unwrap();

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "*.rs", "path": "link-src"}),
        )
        .await;
        assert!(!r.is_error, "{}", r.output);
        assert!(r.output.contains("main.rs"), "{}", r.output);
    }

    #[tokio::test]
    async fn question_mark_matches_exactly_one_char() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({"pattern": "README.m?"})).await;
        assert!(r.output.contains("README.md"));
        // '?' is exactly one character: two of them cannot match one.
        let r = run(dir.path(), serde_json::json!({"pattern": "README??md"})).await;
        assert!(
            !r.output.contains("README.md"),
            "'?' is one character, not zero"
        );
    }

    #[tokio::test]
    async fn directories_match_by_name_too() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({"pattern": "dee*"})).await;
        let root = dir.path().display().to_string();
        assert_eq!(r.output, format!("{root}/src/deep\n"));
    }

    #[tokio::test]
    async fn caps_results_with_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "").unwrap();
        }

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "*.txt", "max_results": 4}),
        )
        .await;
        assert!(r.output.contains("[truncated: stopped after 4 matches"));
        assert!(r.output.matches('\n').count() == 5, "4 paths + the notice");
    }

    #[tokio::test]
    async fn no_match_is_clean_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({"pattern": "*.zzz"})).await;
        assert!(!r.is_error);
        assert_eq!(r.output, "(no matches)\n");
    }

    #[tokio::test]
    async fn missing_or_file_root_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "*", "path": "nope"}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("not found"));

        let r = run(
            dir.path(),
            serde_json::json!({"pattern": "*", "path": "README.md"}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("not a directory"));
    }

    /// Esc before the call: the walk never starts, and the answer says so
    /// as an error rather than as a confident "(no matches)".
    #[tokio::test]
    async fn a_cancelled_search_reports_aborted_without_searching() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let flag = Arc::new(AtomicBool::new(true));

        let r = run_with(
            dir.path(),
            serde_json::json!({"pattern": "*.rs"}),
            &Cancel::new(flag),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("aborted by user"), "{}", r.output);
    }

    /// Esc mid-walk: a stop request raised while the walk is in flight
    /// ends it. The walk is synchronous, so the flag comes from a real
    /// thread, and its delay is a quarter of this machine's measured cost
    /// of the same walk — the stop lands inside the walk here as surely as
    /// on a faster or slower box. The partial hit list is the proof it
    /// really stopped early: the same call uncancelled finds them all.
    #[tokio::test]
    async fn a_cancelled_walk_stops_and_reports_partial_results() {
        let dir = tempfile::tempdir().unwrap();
        let (dirs, per_dir) = (2000usize, 10usize);
        for i in 0..dirs {
            let sub = dir.path().join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            for j in 0..per_dir {
                std::fs::write(sub.join(format!("f{j}.rs")), "").unwrap();
            }
        }
        let total = dirs * per_dir;
        // A cap above the tree's size: an uncancelled walk finds all of them.
        let args = || serde_json::json!({"pattern": "*.rs", "max_results": total + 10});

        let measured = std::time::Instant::now();
        let complete = run(dir.path(), args()).await;
        let walk_time = measured.elapsed();
        assert!(!complete.is_error);
        assert_eq!(complete.output.matches('\n').count(), total);

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
            run_with(dir.path(), args(), &Cancel::new(flag)),
        )
        .await
        .expect("a cancelled walk must not run on to the end of the tree");
        setter.join().unwrap();

        assert!(r.is_error, "a stopped walk is an error: {}", r.output);
        assert!(r.output.contains("aborted by user"), "{}", r.output);
        assert!(r.output.contains("partial"), "{}", r.output);
        let found = r.output.lines().filter(|l| l.ends_with(".rs")).count();
        assert!(
            found > 0 && found < total,
            "the walk must stop partway: found {found} of {total}"
        );
    }
}
