//! Tests for the tool surface: the registry's dispatch (sequential and
//! parallel, with and without a stop request), the canonical name list,
//! and the request's declarations.
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

    fn execute<'a>(&'a self, args: &'a Value, _cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
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
        .dispatch(
            &call("echo", serde_json::json!({"text": "hello"})),
            &Cancel::none(),
        )
        .await;
    assert!(!result.is_error);
    assert_eq!(result.output, "hello");
}

/// A stopped run never starts another tool: the check lives in
/// dispatch, not in each tool remembering to make it.
#[tokio::test]
async fn dispatch_skips_the_tool_once_the_user_stopped_the_run() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let cancel = Cancel::new(Arc::new(AtomicBool::new(true)));

    let result = registry
        .dispatch(&call("echo", serde_json::json!({"text": "x"})), &cancel)
        .await;
    assert!(result.is_error);
    assert_eq!(result.output, "aborted by user");
}

/// A stop request already in effect answers every call of a parallel
/// batch without running any of them — the rule sequential dispatch
/// has, because the batch is made of the same calls.
#[tokio::test]
async fn a_stopped_run_starts_nothing_from_a_parallel_batch() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let cancel = Cancel::new(Arc::new(AtomicBool::new(true)));
    let calls = [
        call("echo", serde_json::json!({"text": "a"})),
        call("echo", serde_json::json!({"text": "b"})),
    ];

    let results = registry.dispatch_parallel(&calls, &cancel).await;
    assert_eq!(results.len(), 2, "every call still gets its result");
    assert!(
        results
            .iter()
            .all(|r| r.is_error && r.output == "aborted before execution"),
        "{results:?}"
    );
}

/// With nothing stopped, a parallel batch runs every call and reports
/// them in the order they were made, whatever order they finish in.
#[tokio::test]
async fn a_parallel_batch_runs_every_call_and_keeps_their_order() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let calls = [
        call("echo", serde_json::json!({"text": "first"})),
        call("echo", serde_json::json!({"text": "second"})),
    ];

    let results = registry.dispatch_parallel(&calls, &Cancel::none()).await;
    let outputs: Vec<&str> = results.iter().map(|r| r.output.as_str()).collect();
    assert_eq!(outputs, ["first", "second"]);
    assert!(results.iter().all(|r| !r.is_error), "{results:?}");
}

#[test]
fn the_default_registry_matches_the_canonical_name_list() {
    let registry = default_registry(std::env::temp_dir(), 30);
    let names: Vec<&str> = registry.tools().iter().map(|tool| tool.name()).collect();
    assert_eq!(names, NAMES);
    assert!(registry.is_read_only("outline"));
}

#[tokio::test]
async fn dispatch_reports_bad_args_as_an_error_result() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let result = registry
        .dispatch(
            &call("echo", serde_json::json!({"nope": 1})),
            &Cancel::none(),
        )
        .await;
    assert!(result.is_error);
    assert!(result.output.contains("invalid arguments"));
}

#[tokio::test]
async fn dispatch_reports_unknown_tools_without_panicking() {
    let registry = Registry::new();

    let result = registry
        .dispatch(&call("ghost", serde_json::json!({})), &Cancel::none())
        .await;
    assert!(result.is_error);
    assert_eq!(result.output, "unknown tool: ghost");
}

#[tokio::test]
async fn unparseable_arguments_are_an_error_result_without_invoking_the_tool() {
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let result = registry
        .dispatch(
            &ToolCall {
                id: "call_x".into(),
                name: "echo".into(),
                args: serde_json::json!({}),
                args_error: Some("expected value at line 1 column 1: `{not json`".into()),
            },
            &Cancel::none(),
        )
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
