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
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

pub struct GrepTool {
    cwd: PathBuf,
}

impl GrepTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: Option<&str>) -> PathBuf {
        super::resolve_path(&self.cwd, path.unwrap_or("."))
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
    files_only: Option<bool>,
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
    let mut search = Search::new(
        cwd,
        regex,
        glob,
        context,
        cap,
        args.files_only.unwrap_or(false),
        cancel,
    );

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
        "Search file contents by regex, or literal text with fixed_strings. Respects \
.gitignore and skips .git, target, node_modules and binaries. Results are grouped by file; \
supports glob, case_insensitive, context lines and files_only; caps at max_results \
(default 50) and 16 KiB of output."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex, or literal text when fixed_strings is true"},
                "path": {"type": "string", "description": "File or directory to search, relative to the cwd, absolute, or starting with ~ (default \".\")"},
                "glob": {"type": "string", "description": "Glob filter on file names or paths, e.g. \"*.rs\""},
                "case_insensitive": {"type": "boolean", "description": "Case-insensitive matching (default: false)"},
                "fixed_strings": {"type": "boolean", "description": "Treat pattern as literal text, not regex (default: false)"},
                "context": {"type": "integer", "description": "Context lines before and after each match (max 10)"},
                "max_results": {"type": "integer", "description": "Maximum matching lines to return (default 50)"},
                "files_only": {"type": "boolean", "description": "Emit each matching file with its match count instead of lines (default: false)"}
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
