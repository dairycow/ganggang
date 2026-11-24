//! The tool surface: one trait, a registry, and dispatch.
//!
//! Tools are plain values in and out. A failed tool is a *value* (`is_error`)
//! fed back to the model — only transport and session IO may abort a run.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::session::ToolCall;

// bash is pub(crate): the TUI's `!` shell shares its bounded runner.
pub(crate) mod bash;
mod edit;
mod find;
mod grep;
mod ls;
mod read;
mod write;

/// What a tool produced. `is_error` results go back to the model verbatim;
/// they never propagate as Rust errors.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(output: String) -> Self {
        Self {
            output,
            is_error: false,
        }
    }

    pub fn err(output: String) -> Self {
        Self {
            output,
            is_error: true,
        }
    }
}

/// Boxed future for `dyn` tool execution. `async fn` in traits is stable but
/// not dyn-compatible, and this alias costs less than an `async-trait`
/// dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The user's stop request, handed to every tool call.
///
/// An `Arc<AtomicBool>` wrapper over the loop's abort flag: a tool whose
/// action can run a long time (a shell command, a directory walk) polls it
/// and stops, so Esc is not dead until the next call boundary. The check is
/// one acquire load — it pairs with the loop's release store, so a tool on
/// another thread sees a stop request as soon as it is made — and it is
/// cheap enough to repeat inside a walk.
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// A flag nothing ever sets, for a caller that must supply a [`Cancel`]
    /// but has no cancellation path of its own (the TUI's `!` shell).
    pub fn none() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// JSON Schema for the `parameters` object of the function declaration.
    fn schema(&self) -> Value;

    /// Whether this tool is read-only and safe to run concurrently with other
    /// read-only tools in the same batch.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Run one call. `cancel` is the user's stop request: a tool with an
    /// unbounded or long-running action checks it and stops promptly,
    /// answering with an error result that says it was aborted. It lives as
    /// long as the call's future, so a tool may hold it across an await.
    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult>;
}

/// Ordered tool set. Order of registration is the order of advertisement.
#[derive(Default)]
pub struct Registry(Vec<Arc<dyn Tool>>);

impl Registry {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.0.push(Arc::from(tool));
    }

    /// Drop tools the allowlist does not name. The caller (CLI/config,
    /// spec §10) validates names beforehand — unknown names never reach
    /// this quietly shrinking the registry.
    pub fn retain(&mut self, mut keep: impl FnMut(&Arc<dyn Tool>) -> bool) {
        self.0.retain(|t| keep(t));
    }

    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.0
    }

    pub fn is_read_only(&self, name: &str) -> bool {
        self.0
            .iter()
            .find(|t| t.name() == name)
            .is_some_and(|t| t.is_read_only())
    }

    /// The request `tools` array: OpenAI function declarations.
    pub fn declarations(&self) -> Vec<Value> {
        self.0
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.schema(),
                    }
                })
            })
            .collect()
    }

    /// Execute one call. A stop request already in effect answers without
    /// touching the tool; unparseable arguments (checked first), unknown
    /// names, and field-level argument problems are error *results* — the
    /// model can read them and retry.
    pub async fn dispatch(&self, call: &ToolCall, cancel: &Cancel) -> ToolResult {
        if let Some(detail) = &call.args_error {
            return ToolResult::err(format!("arguments were not valid JSON: {detail}"));
        }
        let Some(tool) = self.0.iter().find(|t| t.name() == call.name) else {
            return ToolResult::err(format!("unknown tool: {}", call.name));
        };
        if cancel.is_cancelled() {
            // Before the tool, not inside it: an Esc must not pay for one
            // more command's startup.
            return ToolResult::err("aborted by user".into());
        }
        tool.execute(&call.args, cancel).await
    }

    /// Execute a slice of calls concurrently (bounded at 8 in-flight),
    /// returning results in the exact original call order. Every call gets
    /// the user's stop request: one that has not started when it lands is
    /// answered without running, and one already running sees it through
    /// the same [`Cancel`] a sequential call would.
    pub async fn dispatch_parallel(&self, calls: &[ToolCall], cancel: &Cancel) -> Vec<ToolResult> {
        let sem = Arc::new(tokio::sync::Semaphore::new(8));
        let mut set = tokio::task::JoinSet::new();
        for (idx, call) in calls.iter().enumerate() {
            if let Some(detail) = &call.args_error {
                let msg = format!("arguments were not valid JSON: {detail}");
                set.spawn(async move { (idx, ToolResult::err(msg)) });
                continue;
            }
            let Some(tool) = self.0.iter().find(|t| t.name() == call.name).cloned() else {
                let msg = format!("unknown tool: {}", call.name);
                set.spawn(async move { (idx, ToolResult::err(msg)) });
                continue;
            };
            let args = call.args.clone();
            let sem = sem.clone();
            let cancel = cancel.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await;
                if cancel.is_cancelled() {
                    return (idx, ToolResult::err("aborted before execution".into()));
                }
                let res = tool.execute(&args, &cancel).await;
                (idx, res)
            });
        }
        let mut out: Vec<Option<ToolResult>> = vec![None; calls.len()];
        while let Some(joined) = set.join_next().await {
            if let Ok((idx, res)) = joined
                && let Some(slot) = out.get_mut(idx)
            {
                *slot = Some(res);
            }
        }
        out.into_iter()
            .map(|r| r.unwrap_or_else(|| ToolResult::err("tool task panicked".into())))
            .collect()
    }
}

/// Deserialize a tool's arguments permissively into its own args struct,
/// reporting field problems as an error result string.
pub(crate) fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| format!("invalid arguments: {e}"))
}

/// Render a path for tool output without `.` noise: a search rooted at
/// `.` must report `sub/f.txt`, not `./sub/f.txt`. Lexical only — `..`
/// is preserved and the filesystem is not consulted.
pub(crate) fn display(path: &std::path::Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        if let std::path::Component::CurDir = component {
            continue;
        }
        out.push(component.as_os_str());
    }
    out.display().to_string()
}

pub(crate) const PRUNED_DIRS: &[&str] = &[".git", "target", "node_modules"];

pub(crate) fn display_relative(cwd: &std::path::Path, path: &std::path::Path) -> String {
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let rendered = display(rel);
    if rendered.is_empty() {
        ".".to_string()
    } else {
        rendered
    }
}

pub(crate) fn resolve_walk_root(root: &std::path::Path) -> (PathBuf, bool) {
    let is_symlink = std::fs::symlink_metadata(root)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        (
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            true,
        )
    } else {
        (root.to_path_buf(), false)
    }
}

/// Names of the default registry's tools, in advertisement order. The
/// config allowlist and hook matchers validate against this canonical
/// list; registration and this constant move together (pinned by test).
pub(crate) const NAMES: &[&str] = &["read", "bash", "edit", "write", "grep", "find", "ls"];

/// The default tool set, anchored at the session cwd. The `--tools` flag and
/// config allowlist filter this registry (M2.11).
pub fn default_registry(cwd: PathBuf, bash_timeout_secs: u64) -> Registry {
    let mut registry = Registry::new();
    registry.register(Box::new(read::ReadTool::new(cwd.clone())));
    registry.register(Box::new(bash::BashTool::new(
        cwd.clone(),
        bash_timeout_secs,
    )));
    registry.register(Box::new(edit::EditTool::new(cwd.clone())));
    registry.register(Box::new(write::WriteTool::new(cwd.clone())));
    registry.register(Box::new(grep::GrepTool::new(cwd.clone())));
    registry.register(Box::new(find::FindTool::new(cwd.clone())));
    registry.register(Box::new(ls::LsTool::new(cwd)));
    registry
}

#[cfg(test)]
mod tests;
