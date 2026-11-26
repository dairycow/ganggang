//! Esc while a tool is *running*: the loop hands every dispatch the same
//! stop request the tools poll, so a long command dies inside the call
//! instead of at the next call boundary (§9's abort, GAP 3).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::fake::{FakeServer, text_response, tool_call_response};
use super::super::{drive_full, event_values, messages_of};
use crate::agent::hooks::{Hook, HookEvent, Hooks};
use crate::agent::session::Role;
use crate::agent::tools::Registry;
use crate::agent::tools::bash::BashTool;

/// One `bash` call, and a second turn the aborted run must never ask for.
fn one_bash_call(command: &str) -> (FakeServer, Registry) {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "bash", &json!({"command": command}).to_string())]),
        text_response("never asked"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(BashTool::new(
        std::env::temp_dir(),
        300,
        crate::nested::ParentEnv::default().shared(),
    )));
    (server, registry)
}

/// Esc mid-command: the `sleep 30` dies with its process group, its result
/// is recorded, and the run ends with the abort shape without a second
/// request the user has already disowned.
#[tokio::test]
async fn abort_during_a_running_command_kills_it_and_ends_the_run() {
    // The marker doubles as the (unique, valid) sleep duration so pgrep can
    // find the killed group's members.
    let pid = std::process::id();
    let marker = format!("712.{pid}");
    let pattern = format!(r"712\.{pid}([^0-9]|$)");
    let (server, registry) = one_bash_call(&format!("echo starting; sleep {marker}"));

    let abort = Arc::new(AtomicBool::new(false));
    let setter = {
        let abort = abort.clone();
        tokio::spawn(async move {
            // Long enough for the call to be dispatched and the shell to be
            // running, short enough to land well inside the command.
            tokio::time::sleep(Duration::from_millis(300)).await;
            abort.store(true, Ordering::Relaxed);
        })
    };

    let started = Instant::now();
    let (result, entries, events) = tokio::time::timeout(
        Duration::from_secs(10),
        drive_full(
            &server.base_url,
            registry,
            "run it",
            abort,
            None,
            false,
            Hooks::default(),
        ),
    )
    .await
    .expect("the run must not wait out the command");
    setter.await.unwrap();

    assert!(result.is_ok(), "abort is a clean stop, not an error");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the killed command must not be waited on: {:?}",
        started.elapsed()
    );

    let values = event_values(&events);
    let finished = values
        .iter()
        .find(|e| e["type"] == "tool_call_finished")
        .expect("the cancelled call still gets its result event");
    assert_eq!(finished["is_error"], true);
    assert!(
        finished["output"]
            .as_str()
            .unwrap()
            .contains("aborted by user and killed"),
        "{}",
        finished["output"]
    );
    assert!(
        !values.iter().any(|e| e["type"] == "run_ended"),
        "an aborted run has no RunEnded"
    );
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));
    assert!(
        values
            .iter()
            .any(|e| e["type"] == "warned" && e["message"].as_str().unwrap() == "run aborted")
    );

    // The session shape stays replay-safe: the call is answered, and the
    // run closes with the user-visible marker.
    let results: Vec<_> = entries
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .collect();
    assert_eq!(results.len(), 1, "the cancelled call has its tool_result");
    assert!(results[0].is_error);
    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.role, Role::User);
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));

    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "no second request after the stop"
    );

    // Nothing of the killed command's process group is left behind.
    for _ in 0..20 {
        let alive = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(&pattern)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("process-group members survived the abort");
}

/// A `post_tool` hook observes executions, not the user's veto: a call cut
/// short by Esc is reported to the model but never handed to a hook — the
/// same rule as a call that never ran.
#[tokio::test]
async fn a_post_hook_does_not_fire_for_a_cancelled_call() {
    let dir = tempfile::tempdir().unwrap();
    let posted = dir.path().join("post");
    let (server, registry) = one_bash_call("sleep 30");

    let abort = Arc::new(AtomicBool::new(false));
    let setter = {
        let abort = abort.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            abort.store(true, Ordering::Relaxed);
        })
    };
    let hooks = Hooks::new(vec![Hook {
        event: HookEvent::PostTool,
        tools: None,
        command: format!("touch {}", posted.display()),
        timeout_secs: 10,
    }]);

    let (result, _, events) = tokio::time::timeout(
        Duration::from_secs(10),
        drive_full(
            &server.base_url,
            registry,
            "run it",
            abort,
            None,
            false,
            hooks,
        ),
    )
    .await
    .expect("the run must not wait out the command");
    setter.await.unwrap();

    assert!(result.is_ok());
    assert!(
        event_values(&events)
            .iter()
            .any(|e| e["type"] == "tool_call_finished")
    );
    assert!(!posted.exists(), "a cancelled call is not an execution");
}
