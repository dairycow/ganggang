//! The tool surface: one trait, a registry, and dispatch.
//!
//! Tools are plain values in and out. A failed tool is a *value* (`is_error`)
//! fed back to the model — only transport and session IO may abort a run.

use std::path::PathBuf;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::session::ToolCall;

// bash is pub(crate): the TUI's `!` shell shares its bounded runner.
pub(crate) mod bash;
pub(crate) mod ctx;
mod edit;
mod find;
mod grep;
mod ls;
mod outline;
mod path;
mod read;
pub(crate) mod task;
mod write;

pub(crate) use path::{display, display_relative, resolve_path, resolve_walk_root};

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

pub use ctx::{BoxFuture, Cancel, ToolCtx};

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

    /// Run one call with the run's full context. The default delegates to
    /// [`Self::execute`] with the ctx's stop request, so tools that need no events
    /// or call id are untouched; a tool that observes the run (the `task`
    /// tool) overrides it.
    fn execute_with<'a>(&'a self, args: &'a Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolResult> {
        self.execute(args, &ctx.cancel)
    }
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

    /// Execute one call with the user's stop request only (the test-only
    /// form): tools that observe the run get an empty context. The
    /// production loop's dispatch paths use [`dispatch_with`], which hands
    /// the run's full context to the tool.
    #[cfg(test)]
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
        if let Some(rejection) = unknown_args(tool.as_ref(), call) {
            return rejection;
        }
        let ctx = ToolCtx::none(cancel);
        tool.execute_with(&call.args, &ctx).await
    }

    /// Execute one call with the run's full context: tools that observe the
    /// run (the `task` tool's nested events) get the call id and the event
    /// sender instead of a bare stop request. The same rules as the test-only
    /// `dispatch` apply, so the two paths stay one behaviour.
    pub async fn dispatch_with(&self, call: &ToolCall, ctx: &ToolCtx) -> ToolResult {
        if let Some(detail) = &call.args_error {
            return ToolResult::err(format!("arguments were not valid JSON: {detail}"));
        }
        let Some(tool) = self.0.iter().find(|t| t.name() == call.name) else {
            return ToolResult::err(format!("unknown tool: {}", call.name));
        };
        if ctx.cancel.is_cancelled() {
            return ToolResult::err("aborted by user".into());
        }
        if let Some(rejection) = unknown_args(tool.as_ref(), call) {
            return rejection;
        }
        tool.execute_with(&call.args, ctx).await
    }

    /// The test-only parallel form: the stop request only, every call
    /// getting an empty context. The production loop's dispatch paths use
    /// [`dispatch_parallel_with`].
    #[cfg(test)]
    pub async fn dispatch_parallel(&self, calls: &[ToolCall], cancel: &Cancel) -> Vec<ToolResult> {
        let ctx = ToolCtx::none(cancel);
        self.dispatch_parallel_with(calls, &ctx).await
    }

    /// The parallel dispatch with the run's full context: one ctx per call
    /// (its own `call_id`, the shared stop request and event sender), built
    /// inside the spawned task so a tool sees the call it is running.
    pub async fn dispatch_parallel_with(
        &self,
        calls: &[ToolCall],
        ctx: &ToolCtx,
    ) -> Vec<ToolResult> {
        let events = ctx.events.clone();
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
            let cancel = ctx.cancel.clone();
            let call_id = call.id.clone();
            let events = events.clone();
            let rejection = unknown_args(tool.as_ref(), call);
            set.spawn(async move {
                let _permit = sem.acquire_owned().await;
                if cancel.is_cancelled() {
                    return (idx, ToolResult::err("aborted before execution".into()));
                }
                if let Some(rejection) = rejection {
                    return (idx, rejection);
                }
                let ctx = ToolCtx {
                    cancel,
                    events,
                    call_id,
                };
                let res = tool.execute_with(&args, &ctx).await;
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

/// Reject arguments naming parameters the tool's schema does not declare.
/// A silently dropped key is the worse failure: `grep` called with `-i`
/// would simply run case-sensitively. The answer names what was unknown
/// and what is valid, so the model can retry in one go. Arguments that are
/// not an object, and schemas without `properties`, fall through to the
/// tool's own parse error.
fn unknown_args(tool: &dyn Tool, call: &ToolCall) -> Option<ToolResult> {
    let Value::Object(args) = &call.args else {
        return None;
    };
    let schema = tool.schema();
    let props = schema.get("properties")?.as_object()?;
    let mut unknown: Vec<&String> = args
        .keys()
        .filter(|key| !props.contains_key(key.as_str()))
        .collect();
    if unknown.is_empty() {
        return None;
    }
    unknown.sort();
    let mut valid: Vec<&str> = props.keys().map(String::as_str).collect();
    valid.sort();
    let unknown = unknown
        .iter()
        .map(|key| format!("{key:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(ToolResult::err(format!(
        "invalid arguments for {}: unknown parameter(s) {unknown} (valid: {})",
        tool.name(),
        valid.join(", ")
    )))
}

/// Deserialize a tool's arguments into its own args struct,
/// reporting field problems as an error result string.
pub(crate) fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| format!("invalid arguments: {e}"))
}

pub(crate) const PRUNED_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Names of the default registry's tools, in advertisement order.
/// Validation of the config allowlist and hook matchers goes through
/// [`is_known_name`] — this list plus `task` — and this constant stays
/// pinned to the registration order by test.
pub(crate) const NAMES: &[&str] = &[
    "read", "bash", "edit", "write", "grep", "find", "ls", "outline",
];

/// Every tool name the boot registry can advertise: the default registry's
/// tools plus `task`, which the boot path registers separately.
pub(crate) fn is_known_name(name: &str) -> bool {
    NAMES.contains(&name) || name == "task"
}

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
    registry.register(Box::new(ls::LsTool::new(cwd.clone())));
    registry.register(Box::new(outline::OutlineTool::new(cwd)));
    registry
}

#[cfg(test)]
mod tests;
