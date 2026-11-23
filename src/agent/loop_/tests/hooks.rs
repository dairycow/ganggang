//! Scripted-model tests for the §22 loop wiring: where hooks fire, what
//! the model sees, and which failures are warnings rather than aborts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use super::fake::{FakeServer, text_response, tool_call_response};
use super::{Echo, SetFlagThenEcho, Touch, drive_full, drive_hooks, event_values};
use crate::agent::hooks::{Hook, HookEvent, Hooks};
use crate::agent::session::{Entry, Message, Role};
use crate::agent::tools::Registry;

fn hook(event: HookEvent, tools: Option<Vec<&str>>, command: &str) -> Hook {
    Hook {
        event,
        tools: tools.map(|tools| tools.into_iter().map(str::to_string).collect()),
        command: command.to_string(),
        timeout_secs: 10,
    }
}

fn deny(reason: &str) -> String {
    format!(r#"printf '{{"decision":"deny","reason":"{reason}"}}'"#)
}

fn tool_results(entries: &[Entry]) -> Vec<&Message> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message { message, .. } if matches!(message.role, Role::ToolResult) => {
                Some(message)
            }
            _ => None,
        })
        .collect()
}

fn warnings(events: &[crate::events::AgentEvent]) -> Vec<String> {
    event_values(events)
        .into_iter()
        .filter_map(|event| {
            (event["type"] == "warned").then(|| event["message"].as_str().unwrap().to_string())
        })
        .collect()
}

#[tokio::test]
async fn a_pre_tool_deny_prevents_the_side_effect_and_reaches_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        Some(vec!["touch"]),
        &deny("nope"),
    )]);

    let (result, entries, events) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert!(!marker.exists(), "the tool must not have run");
    let results = tool_results(&entries);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].content.as_deref(),
        Some("denied by pre_tool hook: nope")
    );
    assert!(
        event_values(&events)
            .iter()
            .any(|e| e["type"] == "tool_call_finished" && e["is_error"] == true),
        "the interface sees the denial as a failed tool call"
    );
}

#[tokio::test]
async fn post_and_run_end_hooks_fire_after_the_result_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let touched = dir.path().join("tool");
    let posted = dir.path().join("post");
    let ended = dir.path().join("end");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(touched.clone())));
    let hooks = Hooks::new(vec![
        hook(
            HookEvent::PostTool,
            Some(vec!["touch"]),
            &format!("touch {}", posted.display()),
        ),
        hook(
            HookEvent::RunEnd,
            None,
            &format!("touch {}", ended.display()),
        ),
    ]);

    let (result, entries, events) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert!(touched.exists());
    assert!(posted.exists(), "post_tool ran");
    assert!(ended.exists(), "run_end ran");
    assert!(warnings(&events).is_empty());
    // The session still records the result before the post hook ran.
    assert_eq!(
        tool_results(&entries)[0].content.as_deref(),
        Some("touched")
    );
}

#[tokio::test]
async fn a_post_hook_does_not_fire_for_a_denied_call() {
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("tool");
    let posted = dir.path().join("post");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(tool.clone())));
    let hooks = Hooks::new(vec![
        hook(HookEvent::PreTool, None, &deny("no")),
        hook(
            HookEvent::PostTool,
            None,
            &format!("touch {}", posted.display()),
        ),
    ]);

    let (result, _, _) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert!(!tool.exists());
    assert!(!posted.exists());
}

#[tokio::test]
async fn a_broken_pre_hook_warns_and_the_tool_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "exit 7")]);

    let (result, _, events) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert!(marker.exists(), "fail-open: the tool ran");
    let warnings = warnings(&events);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("pre_tool hook"));
    assert!(warnings[0].contains("exit 7"));
}

#[tokio::test]
async fn the_first_deny_wins() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let hooks = Hooks::new(vec![
        hook(HookEvent::PreTool, None, &deny("first")),
        hook(HookEvent::PreTool, None, &deny("second")),
    ]);

    let (result, entries, _) = drive_hooks(&server.base_url, Registry::new(), "go", hooks).await;

    assert!(result.is_ok());
    assert_eq!(
        tool_results(&entries)[0].content.as_deref(),
        Some("denied by pre_tool hook: first")
    );
}

#[tokio::test]
async fn a_broken_post_hook_cannot_change_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{}")]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    let hooks = Hooks::new(vec![hook(HookEvent::PostTool, None, "exit 9")]);

    let (result, entries, events) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert_eq!(
        tool_results(&entries)[0].content.as_deref(),
        Some("touched")
    );
    assert!(
        warnings(&events)
            .iter()
            .any(|w| w.contains("post_tool hook"))
    );
}

#[tokio::test]
async fn unparseable_arguments_skip_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let pre_marker = dir.path().join("pre-ran");
    let post_marker = dir.path().join("post-ran");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "touch", "{not json")]),
        text_response("done"),
    ]);
    let hooks = Hooks::new(vec![
        hook(
            HookEvent::PreTool,
            None,
            &format!("touch {}", pre_marker.display()),
        ),
        hook(
            HookEvent::PostTool,
            None,
            &format!("touch {}", post_marker.display()),
        ),
    ]);

    let (result, entries, _) = drive_hooks(&server.base_url, Registry::new(), "go", hooks).await;

    assert!(result.is_ok());
    assert!(
        !pre_marker.exists(),
        "no hook decision can mask the args error"
    );
    assert!(!post_marker.exists(), "an unexecuted call has no post hook");
    assert!(
        tool_results(&entries)[0]
            .content
            .as_deref()
            .unwrap()
            .starts_with("arguments were not valid JSON")
    );
}

#[tokio::test]
async fn an_abort_during_a_pre_hook_ends_the_run_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched");
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        flag.store(true, Ordering::Relaxed);
    });
    let server = FakeServer::spawn(vec![tool_call_response(&[("call_1", "touch", "{}")])]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "sleep 5")]);

    let started = Instant::now();
    let (result, entries, events) = drive_full(
        &server.base_url,
        registry,
        "go",
        abort.clone(),
        None,
        false,
        hooks,
    )
    .await;

    assert!(result.is_ok());
    assert!(started.elapsed().as_secs() < 3, "abort must kill the hook");
    assert!(!marker.exists(), "the denied tool never ran");
    let results = tool_results(&entries);
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].content.as_deref(),
        Some("aborted before execution")
    );
    assert!(
        event_values(&events)
            .iter()
            .any(|e| e["type"] == "run_aborted")
    );
    assert!(
        !event_values(&events)
            .iter()
            .any(|e| e["type"] == "run_ended")
    );
}

#[tokio::test]
async fn one_denied_call_does_not_stop_the_rest_of_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[
            ("call_1", "touch", "{}"),
            ("call_2", "echo", r#"{"text":"hi"}"#),
        ]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Touch(marker.clone())));
    registry.register(Box::new(Echo));
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        Some(vec!["touch"]),
        &deny("no"),
    )]);

    let (result, entries, _) = drive_hooks(&server.base_url, registry, "go", hooks).await;

    assert!(result.is_ok());
    assert!(!marker.exists());
    let results = tool_results(&entries);
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].content.as_deref(),
        Some("denied by pre_tool hook: no")
    );
    assert_eq!(results[1].content.as_deref(), Some("hi"));
}

#[tokio::test]
async fn a_transport_failure_skips_run_end() {
    let dir = tempfile::tempdir().unwrap();
    let ended = dir.path().join("end");
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "echo", r#"{"text":"x"}"#)]),
        "not json".into(),
    ]);
    let hooks = Hooks::new(vec![hook(
        HookEvent::RunEnd,
        None,
        &format!("touch {}", ended.display()),
    )]);

    let (result, _, events) = drive_hooks(&server.base_url, Registry::new(), "go", hooks).await;

    assert!(
        result.is_err(),
        "the malformed response is a transport error"
    );
    assert!(!ended.exists(), "run_end observes clean finishes only");
    assert!(
        !event_values(&events)
            .iter()
            .any(|e| e["type"] == "run_ended")
    );
}

#[tokio::test]
async fn an_aborted_run_fires_no_run_end_hook() {
    let dir = tempfile::tempdir().unwrap();
    let ended = dir.path().join("end");
    let abort = Arc::new(AtomicBool::new(false));
    let server = FakeServer::spawn(vec![tool_call_response(&[(
        "call_1",
        "echo",
        r#"{"text":"x"}"#,
    )])]);
    let mut registry = Registry::new();
    registry.register(Box::new(SetFlagThenEcho(abort.clone())));
    let hooks = Hooks::new(vec![hook(
        HookEvent::RunEnd,
        None,
        &format!("touch {}", ended.display()),
    )]);

    let (result, _, events) = drive_full(
        &server.base_url,
        registry,
        "go",
        abort.clone(),
        None,
        false,
        hooks,
    )
    .await;

    assert!(result.is_ok());
    assert!(abort.load(Ordering::Relaxed));
    assert!(!ended.exists(), "run_end observes clean finishes only");
    assert!(
        event_values(&events)
            .iter()
            .any(|e| e["type"] == "run_aborted")
    );
    assert!(
        !event_values(&events)
            .iter()
            .any(|e| e["type"] == "run_ended")
    );
}

#[tokio::test]
async fn the_session_id_rides_in_the_payload() {
    use crate::agent::session::io::SessionStore;

    let dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::create_under(root.path(), dir.path(), "fake-model").unwrap();
    let expected = store.id().to_string();
    let server = FakeServer::spawn(vec![text_response("done")]);
    let end = dir.path().join("end.json");
    let hooks = Hooks::new(vec![hook(
        HookEvent::RunEnd,
        None,
        &format!("cat > {}", end.display()),
    )]);

    let (result, _, _) = super::drive_full(
        &server.base_url,
        Registry::new(),
        "go",
        Arc::new(AtomicBool::new(false)),
        Some(store),
        false,
        hooks,
    )
    .await;

    assert!(result.is_ok());
    let seen: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&end).unwrap()).unwrap();
    assert_eq!(seen["session_id"], json!(expected));
}

#[tokio::test]
async fn hook_warnings_precede_run_ended() {
    let server = FakeServer::spawn(vec![text_response("done")]);
    let hooks = Hooks::new(vec![hook(HookEvent::RunEnd, None, "exit 1")]);

    let (result, _, events) = drive_hooks(&server.base_url, Registry::new(), "go", hooks).await;

    assert!(result.is_ok());
    let values = event_values(&events);
    let warned = values
        .iter()
        .position(|e| e["type"] == "warned")
        .expect("the failure must be visible");
    let ended = values
        .iter()
        .position(|e| e["type"] == "run_ended")
        .expect("the run still ends");
    assert!(warned < ended);
}
