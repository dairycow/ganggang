//! `grep`: regex/literal content search across files, with `.gitignore`
//! awareness and hard result caps.

use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::Value;

use super::{
    BoxFuture, Cancel, PRUNED_DIRS, Tool, ToolResult, display_relative, parse_args,
    resolve_walk_root,
};

mod search;

use search::Search;

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
    let mut search = Search::new(cwd, regex, glob, context, cap, cancel);

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
    search.report()
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
