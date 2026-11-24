//! Wave 2 & 3 loop tests: `--max-turns` wrap-up & hard stop, frozen
//! system-prompt date, and parallel read-only tool dispatch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::Agent;
use crate::agent::session::Role;
use crate::agent::tools::{self, Registry, Tool, ToolResult, parse_args};
use crate::llm::client::LlmClient;

use super::fake::{FakeServer, text_response, tool_call_response};
use super::{Echo, event_values, messages_of};

struct SlowReadOnlyTool {
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Tool for SlowReadOnlyTool {
    fn name(&self) -> &'static str {
        "slow_ro"
    }
    fn description(&self) -> &'static str {
        "sleep briefly and return label"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"label": {"type": "string"}}, "required": ["label"]})
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn execute<'a>(
        &'a self,
        args: &'a Value,
        _cancel: &'a tools::Cancel,
    ) -> tools::BoxFuture<'a, ToolResult> {
        #[derive(serde::Deserialize)]
        struct Args {
            label: String,
        }
        let active = self.active.clone();
        let peak = self.peak.clone();
        Box::pin(async move {
            let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(cur, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            match parse_args::<Args>(args) {
                Ok(a) => ToolResult::ok(a.label),
                Err(e) => ToolResult::err(e),
            }
        })
    }
}

#[tokio::test]
async fn max_turns_nudges_at_80_pct_and_stops_cleanly_at_limit() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("c1", "echo", r#"{"text":"t1"}"#)]),
        tool_call_response(&[("c2", "echo", r#"{"text":"t2"}"#)]),
        tool_call_response(&[("c3", "echo", r#"{"text":"t3"}"#)]),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    )
    .with_max_turns(Some(3));

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }

    let values = event_values(&events);
    assert!(values.iter().any(|v| {
        v["type"] == "warned"
            && v["message"]
                .as_str()
                .unwrap()
                .contains("approaching max_turns (2/3)")
    }));
    assert!(values.iter().any(|v| {
        v["type"] == "warned"
            && v["message"]
                .as_str()
                .unwrap()
                .contains("reached max_turns (3)")
    }));
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 3})
    );

    // Ephemeral wrap-up nudge rode in requests 2 & 3 on the last tool message,
    // NOT as a new trailing user message and NOT in persisted entries.
    let requests = server.requests.lock().unwrap().clone();
    let req2_last = requests[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(req2_last["role"], "tool");
    assert!(
        req2_last["content"]
            .as_str()
            .unwrap()
            .contains("turn 2 of 3 (1 turn(s) remaining)")
    );
    let req3_last = requests[2]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(req3_last["role"], "tool");
    assert!(
        req3_last["content"]
            .as_str()
            .unwrap()
            .contains("Do not call any tools")
    );
    assert!(
        !agent
            .entries()
            .iter()
            .any(|e| format!("{e:?}").contains("turn 3 of 3"))
    );
    assert_eq!(super::super::soft_nudge_turn(2), 2);
    assert_eq!(super::super::soft_nudge_turn(3), 2);
    assert_eq!(super::super::soft_nudge_turn(4), 3);
    assert_eq!(super::super::soft_nudge_turn(5), 4);
    assert_eq!(super::super::soft_nudge_turn(10), 8);
    assert_eq!(super::super::soft_nudge_turn(40), 32);
    // All 3 tool calls have results.
    let tool_results = agent
        .entries()
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .count();
    assert_eq!(tool_results, 3);
}

#[tokio::test]
async fn parallel_read_only_tools_run_concurrently_and_preserve_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let server = FakeServer::spawn(vec![
        tool_call_response(&[
            ("c1", "slow_ro", r#"{"label":"first"}"#),
            ("c2", "slow_ro", r#"{"label":"second"}"#),
            ("c3", "slow_ro", r#"{"label":"third"}"#),
        ]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(SlowReadOnlyTool {
        active: active.clone(),
        peak: peak.clone(),
    }));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("read three".into(), &mut tx).await.unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "expected concurrent execution, peak={}",
        peak.load(Ordering::SeqCst)
    );

    let outputs: Vec<&str> = agent
        .entries()
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(outputs, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn system_prompt_is_byte_identical_across_turns() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("c1", "echo", r#"{"text":"hi"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests[0]["messages"][0], requests[1]["messages"][0]);
}

#[tokio::test]
async fn prompt_date_stays_frozen_when_overridden() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("c1", "echo", r#"{"text":"hi"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );
    agent.prompt_date = "2000-01-01".to_string();

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    let requests = server.requests.lock().unwrap().clone();
    assert!(
        requests[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("date: 2000-01-01")
    );
    assert_eq!(requests[0]["messages"][0], requests[1]["messages"][0]);
}

#[tokio::test]
async fn mixed_batch_preserves_barrier_ordering() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let server = FakeServer::spawn(vec![
        tool_call_response(&[
            ("c1", "slow_ro", r#"{"label":"r1"}"#),
            ("c2", "slow_ro", r#"{"label":"r2"}"#),
            ("c3", "echo", r#"{"text":"w"}"#),
            ("c4", "slow_ro", r#"{"label":"r3"}"#),
        ]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(SlowReadOnlyTool {
        active: active.clone(),
        peak: peak.clone(),
    }));
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("mix".into(), &mut tx).await.unwrap();
    drop(tx);
    let mut started_order = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let crate::events::AgentEvent::ToolCallStarted { id, .. } = ev {
            started_order.push(id);
        }
    }
    assert_eq!(started_order, vec!["c1", "c2", "c3", "c4"]);
    assert!(peak.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn parallel_pre_tool_interruption_flushes_all_tool_results() {
    let abort = Arc::new(AtomicBool::new(false));
    let server = FakeServer::spawn(vec![tool_call_response(&[
        ("c1", "slow_ro", r#"{"label":"one"}"#),
        ("c2", "slow_ro", r#"{"label":"two"}"#),
        ("c3", "slow_ro", r#"{"label":"three"}"#),
    ])]);
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("interrupt_second.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *'\"two\"'*) sleep 0.2 ;; esac\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let hooks = crate::agent::hooks::Hooks::new(vec![crate::agent::hooks::Hook {
        event: crate::agent::hooks::HookEvent::PreTool,
        tools: None,
        command: script.display().to_string(),
        timeout_secs: 5,
    }]);

    let mut registry = Registry::new();
    registry.register(Box::new(SlowReadOnlyTool {
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
    }));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        abort.clone(),
        dir.path().to_path_buf(),
        None,
        false,
        131_072,
        hooks,
    );

    let abort_trigger = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        abort_trigger.store(true, Ordering::Release);
    });

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }

    let values = event_values(&events);
    assert_eq!(values.last().unwrap()["type"], "run_aborted");
    let started: Vec<&str> = values
        .iter()
        .filter(|v| v["type"] == "tool_call_started")
        .filter_map(|v| v["id"].as_str())
        .collect();
    let finished: Vec<&str> = values
        .iter()
        .filter(|v| v["type"] == "tool_call_finished")
        .filter_map(|v| v["id"].as_str())
        .collect();
    assert_eq!(started, vec!["c1", "c2"]);
    assert_eq!(finished, vec!["c1", "c2"]);

    let results: Vec<Option<&str>> = agent
        .entries()
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .map(|m| m.tool_call_id.as_deref())
        .collect();
    assert_eq!(results, vec![Some("c1"), Some("c2"), Some("c3")]);
}

#[tokio::test]
async fn abort_during_dispatch_parallel_emits_clean_aborts() {
    let abort = Arc::new(AtomicBool::new(false));
    let server = FakeServer::spawn(vec![tool_call_response(&[
        ("c1", "slow_ro", r#"{"label":"one"}"#),
        ("c2", "slow_ro", r#"{"label":"two"}"#),
    ])]);
    let mut registry = Registry::new();
    registry.register(Box::new(SlowReadOnlyTool {
        active: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
    }));
    let client = LlmClient::new(&server.base_url).unwrap();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        abort.clone(),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );

    let abort_trigger = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        abort_trigger.store(true, Ordering::Release);
    });

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    agent.run("go".into(), &mut tx).await.unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }

    let values = event_values(&events);
    assert_eq!(values.last().unwrap()["type"], "run_aborted");
    let results_count = agent
        .entries()
        .iter()
        .map(messages_of)
        .filter(|m| m.role == Role::ToolResult)
        .count();
    assert_eq!(results_count, 2);
}
