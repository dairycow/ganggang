//! `find`: locate entries by glob, `.gitignore`-aware and capped.

use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::Value;

use super::{
    BoxFuture, Cancel, PRUNED_DIRS, Tool, ToolResult, display_relative, parse_args,
    resolve_walk_root,
};

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
    include_ignored: Option<bool>,
}

fn find(cwd: &Path, root: &Path, args: &FindArgs, cancel: &Cancel) -> ToolResult {
    if args.pattern.is_empty() {
        return ToolResult::err("pattern must not be empty".into());
    }
    let matcher = match GlobBuilder::new(&args.pattern).build() {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolResult::err(format!("invalid glob: {e}")),
    };
    let match_path = args.pattern.contains('/');

    let Ok(meta) = std::fs::metadata(root) else {
        return ToolResult::err(format!(
            "cannot search {}: not found",
            display_relative(cwd, root)
        ));
    };
    if !meta.is_dir() {
        return ToolResult::err(format!(
            "cannot search {}: not a directory",
            display_relative(cwd, root)
        ));
    }

    let cap = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
    let include_ignored = args.include_ignored.unwrap_or(false);
    let (walk_root, was_symlink) = resolve_walk_root(root);

    let walker = WalkBuilder::new(&walk_root)
        .hidden(false)
        .git_ignore(!include_ignored)
        .git_global(!include_ignored)
        .git_exclude(!include_ignored)
        .ignore(!include_ignored)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            if !e.file_type().is_some_and(|ft| ft.is_dir()) {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if include_ignored {
                name != ".git"
            } else {
                !PRUNED_DIRS.contains(&name.as_ref())
            }
        })
        .build();

    let mut hits = Vec::new();
    // Checked every entry: the walk runs on a blocking thread, where
    // dropping the call's future cannot reach it — only the stop request
    // can end it early.
    let mut stopped = false;
    for entry in walker.flatten() {
        if cancel.is_cancelled() {
            stopped = true;
            break;
        }
        if entry.depth() == 0 {
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
        let rel = display_relative(cwd, &display_path);
        let matched = if match_path {
            matcher.is_match(&rel)
        } else {
            display_path
                .file_name()
                .is_some_and(|n| matcher.is_match(n))
        };
        if matched {
            hits.push(rel);
            if hits.len() >= cap {
                break;
            }
        }
    }

    let mut out = hits.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    // A stopped walk's answer is an error: its hits are real, but a
    // "(no matches)" from a walk that gave up would read as a miss.
    if stopped {
        return ToolResult::err(if hits.is_empty() {
            "search aborted by user before any match\n".into()
        } else {
            format!("{out}search aborted by user — results are partial\n")
        });
    }
    if out.is_empty() {
        out.push_str("(no matches)\n");
    }
    if hits.len() >= cap {
        out.push_str(&format!(
            "[truncated: stopped after {cap} matches — more may exist]\n"
        ));
    }
    ToolResult::ok(out)
}

impl Tool for FindTool {
    fn name(&self) -> &'static str {
        "find"
    }

    fn description(&self) -> &'static str {
        "Find files and directories by glob (e.g. \"*.rs\", \"**/*.md\"). Respects \
.gitignore and skips .git, target and node_modules by default (include_ignored: true \
to include them). Sorted and capped at max_results (default 50)."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob matched against entry names, or relative paths when it contains '/'"},
                "path": {"type": "string", "description": "Directory to search from (default: the working directory)"},
                "max_results": {"type": "integer", "description": "Maximum paths to return (default 50)"},
                "include_ignored": {"type": "boolean", "description": "Include .gitignore'd, target/ and node_modules/ entries (default: false)"}
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
            let args: FindArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let cwd = self.cwd.clone();
            let root = self.resolve(args.path.as_deref());
            let cancel = cancel.clone();
            match tokio::task::spawn_blocking(move || find(&cwd, &root, &args, &cancel)).await {
                Ok(res) => res,
                Err(e) => ToolResult::err(format!("find task failed: {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests;
