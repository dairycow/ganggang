//! Degradation-path scripts: malformed arguments, abort mid-batch,
//! truncation and empty-content warnings — where the loop must degrade
//! visibly and keep running (§9, §17). The truncated-turn scripts (a
//! `length` finish, a cut stream, the continuation budget) are in
//! [`continuation`], and the ones for a turn that was nothing but
//! reasoning in [`reasoning`]: same contract, their own files, per the
//! module budget.

mod cancel;
mod continuation;
mod reasoning;
mod retry;
mod stream_abort;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::fake::{FakeServer, text_response, tool_call_response};
use super::{
    Echo, Registry, SetFlagThenEcho, Touch, drive, drive_with_abort, drive_with_store,
    event_values, messages_of,
};
use crate::agent::session::Role;

#[tokio::test]
async fn malformed_arguments_warn_and_feed_back_end_to_end() {
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_1", "echo", "{not json")]),
        text_response("fine"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));

    let (result, entries, events) = drive(&server.base_url, registry, "echo something").await;

    assert!(result.is_ok());
    let values = event_values(&events);
    let warned = values.iter().find(|e| e["type"] == "warned").unwrap();
    assert!(
        warned["message"]
            .as_str()
            .unwrap()
            .contains("not valid JSON")
    );
    let finished = values
        .iter()
        .find(|e| e["type"] == "tool_call_finished")
        .unwrap();
    assert_eq!(finished["is_error"], true);
    assert!(
        finished["output"]
            .as_str()
            .unwrap()
            .contains("not valid JSON")
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 2})
    );

    // The replayed call carries {} — never the invalid text. Found by role:
    // a system prompt message sits at [0].
    let requests = server.requests.lock().unwrap().clone();
    let assistant = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let replayed_args = &assistant["tool_calls"][0]["function"]["arguments"];
    assert_eq!(replayed_args, &json!("{}"));
    assert!(messages_of(&entries[2]).is_error);
}

#[tokio::test]
async fn abort_mid_batch_stops_before_next_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("touched.txt");
    let server = FakeServer::spawn(vec![tool_call_response(&[
        ("call_1", "echo", r#"{"text":"first"}"#),
        ("call_2", "touch", r#"{}"#),
    ])]);
    let mut registry = Registry::new();
    let abort = Arc::new(AtomicBool::new(false));
    registry.register(Box::new(SetFlagThenEcho(abort.clone())));
    registry.register(Box::new(Touch(marker.clone())));

    let (result, entries, events) =
        drive_with_abort(&server.base_url, registry, "two things", abort).await;

    assert!(result.is_ok(), "abort is a clean stop, not an error");
    assert!(!marker.exists(), "the second tool must never run");
    let values = event_values(&events);
    assert_eq!(
        values
            .iter()
            .filter(|e| e["type"] == "tool_call_started")
            .count(),
        1,
        "no ToolCallStarted for the aborted call"
    );
    assert!(
        values
            .iter()
            .any(|e| e["type"] == "warned" && e["message"].as_str().unwrap() == "run aborted")
    );
    assert!(
        !values.iter().any(|e| e["type"] == "run_ended"),
        "an aborted run has no RunEnded"
    );
    assert!(
        values.iter().any(|e| e["type"] == "run_aborted"),
        "an aborted run emits its terminal RunAborted event"
    );

    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.role, Role::User);
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));

    // Replay invariant (§8): every tool_call the assistant made — including
    // the aborted one — is followed by a tool_result, so to_wire produces a
    // request no provider rejects.
    let assistant = entries
        .iter()
        .map(messages_of)
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    for call in &assistant.tool_calls {
        let answered = entries.iter().map(messages_of).any(|m| {
            m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some(call.id.as_str())
        });
        assert!(answered, "call {} has no tool_result", call.id);
    }
    let aborted_result = entries
        .iter()
        .map(messages_of)
        .find(|m| m.role == Role::ToolResult && m.tool_call_id.as_deref() == Some("call_2"))
        .unwrap();
    assert!(aborted_result.is_error);
    assert_eq!(
        aborted_result.content.as_deref(),
        Some("aborted before execution")
    );
}

#[tokio::test]
async fn empty_final_answer_emits_a_warning_and_still_ends_cleanly() {
    let body = json!({
        "choices": [{
            "message": {"role": "assistant", "content": null},
            "finish_reason": "stop",
        }],
        "usage": null,
    })
    .to_string();
    let server = FakeServer::spawn(vec![body]);

    let (result, _, events) = drive(&server.base_url, Registry::new(), "hi").await;

    assert!(result.is_ok());
    let values = event_values(&events);
    assert!(
        values
            .iter()
            .any(|e| e["type"] == "warned" && e["message"] == "assistant response had no content"),
        "empty content must warn (§17), got {values:?}"
    );
    assert_eq!(
        values.last().unwrap(),
        &json!({"type": "run_ended", "turns": 1})
    );
}

#[tokio::test]
async fn advertised_skills_ride_in_the_outgoing_system_prompt() {
    use crate::agent::skills::Skill;

    let server = FakeServer::spawn(vec![text_response("hi")]);
    let client = crate::llm::client::LlmClient::new(&server.base_url).unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut agent = crate::agent::Agent::new(
        client,
        Registry::new(),
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        cwd.path().to_path_buf(),
        None,
        false,
        131_072,
        Default::default(),
    );
    agent.skills = vec![Skill {
        name: "pdf-tools".into(),
        description: "Extract text from PDFs".into(),
        disable_model_invocation: false,
        path: cwd.path().join("pdf-tools/SKILL.md"),
        instructions: "body".into(),
    }];

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    let handle = tokio::spawn(async move { agent.run("hi".into(), &mut tx).await });
    while (rx.recv().await).is_some() {}
    assert!(handle.await.unwrap().is_ok());

    let requests = server.requests.lock().unwrap().clone();
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert_eq!(requests[0]["messages"][0]["role"], "system");
    assert!(system.contains("pdf-tools"));
    assert!(system.contains("pdf-tools/SKILL.md"));
}

#[tokio::test]
async fn a_persisted_session_resumes_with_its_history() {
    use crate::agent::session::io::{SessionStore, load};

    // Run 1: a tool round-trip, persisted to a session file.
    let server = FakeServer::spawn(vec![
        tool_call_response(&[("call_9", "echo", r#"{"text":"hi"}"#)]),
        text_response("done"),
    ]);
    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let store = SessionStore::create_under(root.path(), cwd.path(), "fake-model").unwrap();
    let path = store.path().to_path_buf();

    let (result, _, _) = drive_with_store(
        &server.base_url,
        registry,
        "remember this",
        Arc::new(AtomicBool::new(false)),
        Some(store),
    )
    .await;
    assert!(result.is_ok());

    // Run 2 on a fresh server: the resumed Agent replays the loaded
    // history in its outgoing request.
    let server2 = FakeServer::spawn(vec![text_response("resumed")]);
    let client = crate::llm::client::LlmClient::new(&server2.base_url).unwrap();
    let store2 = crate::agent::session::io::SessionStore::open_existing(&path).unwrap();
    let mut agent = crate::agent::Agent::new(
        client,
        Registry::new(),
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        Some(store2),
        false,
        131_072,
        Default::default(),
    );
    let (_, entries, _) = load(&path).unwrap();
    let entry_count = entries.len();
    agent.load_entries(entries);

    let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
    let handle = tokio::spawn(async move { agent.run("next question".into(), &mut tx).await });
    while (rx.recv().await).is_some() {}
    assert!(handle.await.unwrap().is_ok());

    let requests = server2.requests.lock().unwrap().clone();
    let messages = &requests[0]["messages"];
    // system + the four persisted entries + the new user message.
    assert_eq!(messages.as_array().unwrap().len(), 1 + entry_count + 1);
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "remember this");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_9");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(
        messages[1 + entry_count]["content"],
        "next question",
        "the new input lands after the resumed history"
    );
}

#[tokio::test]
async fn a_latched_abort_flag_does_not_poison_the_next_run() {
    let server = FakeServer::spawn(vec![text_response("fresh")]);
    let registry = Registry::new();
    let latched = Arc::new(AtomicBool::new(true));

    let (result, _, events) = drive_with_abort(&server.base_url, registry, "hi", latched).await;

    assert!(result.is_ok());
    assert_eq!(
        event_values(&events).last().unwrap(),
        &json!({"type": "run_ended", "turns": 1}),
        "run() resets the flag at entry — a stale latch must not abort"
    );
}

#[tokio::test]
async fn abort_during_the_final_tool_call_stops_before_the_next_request() {
    let server = FakeServer::spawn(vec![tool_call_response(&[(
        "call_1",
        "echo",
        r#"{"text":"only"}"#,
    )])]);
    let mut registry = Registry::new();
    let abort = Arc::new(AtomicBool::new(false));
    registry.register(Box::new(SetFlagThenEcho(abort.clone())));

    let drove = drive_with_abort(&server.base_url, registry, "one thing", abort);
    let (result, entries, events) = tokio::time::timeout(std::time::Duration::from_secs(10), drove)
        .await
        .expect("run must end without waiting on a second response");

    assert!(result.is_ok());
    let values = event_values(&events);
    assert_eq!(values.last().unwrap(), &json!({"type": "run_aborted"}));
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "no extra request after the user stopped the run"
    );
    let last = messages_of(entries.last().unwrap());
    assert_eq!(last.content.as_deref(), Some("[aborted by user]"));
}
