//! The tool surface: one trait, a registry, and dispatch.
//!
//! Tools are plain values in and out. A failed tool is a *value* (`is_error`)
//! fed back to the model — only transport and session IO may abort a run.

use std::future::Future;
use std::pin::Pin;

use std::path::PathBuf;

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

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    /// JSON Schema for the `parameters` object of the function declaration.
    fn schema(&self) -> Value;

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult>;
}

/// Ordered tool set. Order of registration is the order of advertisement.
#[derive(Default)]
pub struct Registry(Vec<Box<dyn Tool>>);

impl Registry {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.0.push(tool);
    }

    /// Drop tools the allowlist does not name. The caller (CLI/config,
    /// spec §10) validates names beforehand — unknown names never reach
    /// this quietly shrinking the registry.
    pub fn retain(&mut self, keep: impl FnMut(&Box<dyn Tool>) -> bool) {
        self.0.retain(keep);
    }

    pub fn tools(&self) -> &[Box<dyn Tool>] {
        &self.0
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

    /// Execute one call. Unparseable arguments (checked first), unknown
    /// names, and field-level argument problems are error *results* — the
    /// model can read them and retry.
    pub async fn dispatch(&self, call: &ToolCall) -> ToolResult {
        if let Some(detail) = &call.args_error {
            return ToolResult::err(format!("arguments were not valid JSON: {detail}"));
        }
        let Some(tool) = self.0.iter().find(|t| t.name() == call.name) else {
            return ToolResult::err(format!("unknown tool: {}", call.name));
        };
        tool.execute(&call.args).await
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
mod tests {
    use super::*;

    struct Echo;

    #[derive(serde::Deserialize)]
    struct EchoArgs {
        text: String,
    }

    impl Tool for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }

        fn description(&self) -> &'static str {
            "echo the text back"
        }

        fn schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            })
        }

        fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
            Box::pin(async move {
                match parse_args::<EchoArgs>(args) {
                    Ok(a) => ToolResult::ok(a.text),
                    Err(e) => ToolResult::err(e),
                }
            })
        }
    }

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: "call_x".into(),
            name: name.into(),
            args,
            args_error: None,
        }
    }

    #[tokio::test]
    async fn dispatch_runs_a_registered_tool() {
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));

        let result = registry
            .dispatch(&call("echo", serde_json::json!({"text": "hello"})))
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "hello");
    }

    #[test]
    fn the_default_registry_matches_the_canonical_name_list() {
        let registry = default_registry(std::env::temp_dir(), 30);
        let names: Vec<&str> = registry.tools().iter().map(|tool| tool.name()).collect();
        assert_eq!(names, NAMES);
    }

    #[tokio::test]
    async fn dispatch_reports_bad_args_as_an_error_result() {
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));

        let result = registry
            .dispatch(&call("echo", serde_json::json!({"nope": 1})))
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("invalid arguments"));
    }

    #[tokio::test]
    async fn dispatch_reports_unknown_tools_without_panicking() {
        let registry = Registry::new();

        let result = registry
            .dispatch(&call("ghost", serde_json::json!({})))
            .await;
        assert!(result.is_error);
        assert_eq!(result.output, "unknown tool: ghost");
    }

    #[tokio::test]
    async fn unparseable_arguments_are_an_error_result_without_invoking_the_tool() {
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));

        let result = registry
            .dispatch(&ToolCall {
                id: "call_x".into(),
                name: "echo".into(),
                args: serde_json::json!({}),
                args_error: Some("expected value at line 1 column 1: `{not json`".into()),
            })
            .await;
        assert!(result.is_error);
        assert!(result.output.starts_with("arguments were not valid JSON"));
        // If the tool had run with empty args it would have complained about
        // a missing `text` field instead — a different, misleading error.
    }

    #[test]
    fn declarations_use_the_function_shape() {
        let mut registry = Registry::new();
        registry.register(Box::new(Echo));

        let decls = registry.declarations();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["type"], "function");
        assert_eq!(decls[0]["function"]["name"], "echo");
        assert_eq!(decls[0]["function"]["parameters"]["required"][0], "text");
    }
}
