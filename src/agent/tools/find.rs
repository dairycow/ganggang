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

use super::{BoxFuture, Tool, ToolResult, display, parse_args};

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

fn find(root: &Path, pattern: &str, max_results: usize) -> ToolResult {
    if pattern.is_empty() {
        return ToolResult::err("pattern must not be empty".into());
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
    walk(root, &pat, &mut hits, max_results);

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
/// are leaves: following them could cycle. Returns false at the cap so
/// the recursion unwinds.
fn walk(dir: &Path, pat: &[char], hits: &mut Vec<String>, cap: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true; // unreadable subtree: skipped, the search continues
    };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    children.sort();
    for child in children {
        let name: Vec<char> = child
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned())
            .chars()
            .collect();
        if glob(pat, &name) {
            hits.push(display(&child));
            if hits.len() >= cap {
                return false;
            }
        }
        let is_dir = std::fs::symlink_metadata(&child)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir && !walk(&child, pat, hits, cap) {
            return false;
        }
    }
    true
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

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: FindArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            find(
                &self.resolve(args.path.as_deref()),
                &args.pattern,
                args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
        FindTool::new(dir.to_path_buf()).execute(&args).await
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
}
