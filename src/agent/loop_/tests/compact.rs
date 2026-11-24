//! Loop-integration tests for compaction (§14): the trigger's position
//! in the request sequence, the summarize round-trip, and the
//! summary-failure fallback.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::agent::Agent;
use crate::agent::session::{Entry, Role};
use crate::agent::tools::Registry;
use crate::llm::client::LlmClient;

use super::fake::{FakeServer, text_response, tool_call_response};
use super::{Echo, event_values};

/// The scripted tool round-trip, reporting `total` usage tokens. The loop
/// reads the last response's usage to decide whether compaction is due, so
/// these scripts need numbers near the window they are testing.
fn tool_call_with_usage(id: &str, args: &str, total: u32) -> String {
    let response = tool_call_response(&[(id, "echo", args)]);
    let mut value: serde_json::Value = serde_json::from_str(&response).unwrap();
    value["usage"] = serde_json::json!({
        "prompt_tokens": total - 8,
        "completion_tokens": 8,
        "total_tokens": total,
    });
    value.to_string()
}

/// Four echo-tool runs of ~9k entry-tokens each (12k-char user prompt,
/// 12k-char tool arguments, 12k-char tool result) under a 131,072-token
/// window. The usage numbers put the context past the 114,688-token line
/// after every tool batch. The third run crosses the 20,000-token kept
/// budget (27k > 20k) and the fourth compacts AGAIN from the first
/// compaction's boundary: the repeated-compaction rule, exercised
/// end-to-end.
#[tokio::test]
async fn window_scaled_compaction_runs_between_batches() {
    let big = "x".repeat(12_000);
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    let responses = vec![
        // run 0: tool round-trip, nothing to cut yet (9k ≤ 20k)
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        // run 1: 18k, still within budget
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        // run 2: 27k — the cut fires after the batch: summarize, then answer
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("SUMMARY-XYZ"),
        text_response("ok"),
        // run 3: active region regrows past the line — second compaction
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("SUMMARY-2"),
        text_response("ok"),
    ];
    let server = FakeServer::spawn(responses);

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
        131_072, // scaled reserve: 16,384; line 114,688; keep 20,000
        Default::default(),
    );

    let mut events = Vec::new();
    for i in 0..4 {
        let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
        let result = agent.run(format!("run-{i} {big}"), &mut tx).await;
        assert!(result.is_ok(), "run {i} failed: {:?}", result.err());
        drop(tx);
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
    }

    // 2 model requests per run, plus one summarize call in each of runs
    // 2 and 3.
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 10);

    // The summarize calls (requests 5 and 8) lead with §14's instruction
    // and carry the dropped region — never a system prompt. The second
    // one's dropped region is the FIRST compaction's note plus the spans
    // accumulated since (nothing summarized before is lost).
    for (index, first_kept_run) in [(5usize, "run-0"), (8, "run-1")] {
        let summarize = &requests[index];
        assert_eq!(summarize["messages"][0]["role"], "user");
        assert!(
            summarize["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("summarize the conversation so far for continuation"),
            "{}",
            summarize["messages"][0]
        );
        // The dropped region rides along (after the instruction, and
        // after the previous summary note for the second compaction).
        assert!(
            summarize["messages"].to_string().contains(first_kept_run),
            "summarize request {index} must carry the dropped region"
        );
    }
    let second = requests[8]["messages"].to_string();
    assert!(
        second.contains("SUMMARY-XYZ"),
        "the second summarize call sees the first summary: {second}"
    );

    // The final request replays under the newest compaction: summary
    // note first, then only the kept spans.
    let replayed = requests[9]["messages"].to_string();
    assert!(replayed.contains("SUMMARY-2"));
    assert!(replayed.contains("[conversation before this point was compacted]"));
    assert!(replayed.contains("run-2"));
    assert!(replayed.contains("run-3"));
    assert!(
        !replayed.contains("run-0") && !replayed.contains("run-1"),
        "dropped spans must not replay"
    );

    // The session records both compactions; boundaries name user entries;
    // raw entries before them stay in the log.
    let entries = agent.entries();
    let compactions: Vec<&Entry> = entries
        .iter()
        .filter(|e| matches!(e, Entry::Compaction { .. }))
        .collect();
    assert_eq!(compactions.len(), 2);
    for entry in &compactions {
        let Entry::Compaction { first_kept_id, .. } = entry else {
            unreachable!()
        };
        let kept_is_user = entries.iter().any(|e| match e {
            Entry::Message { id, message, .. } => id == first_kept_id && message.role == Role::User,
            _ => false,
        });
        assert!(
            kept_is_user,
            "boundary {first_kept_id} must name a user entry"
        );
    }
    assert!(entries.len() > 12, "raw entries stay in the file");

    // The run stayed visible about it (§17) — twice.
    let values = event_values(&events);
    let compacted = values
        .iter()
        .filter(|v| {
            v["type"] == "warned" && v["message"].as_str().unwrap().contains("context compacted")
        })
        .count();
    assert_eq!(compacted, 2);
}

/// A summarize call that fails (garbage response) degrades to a `Warned`
/// and the run continues un-compacted: full history still replays, and
/// the next trigger retries.
#[tokio::test]
async fn summary_failure_warns_and_continues_un_compacted() {
    let big = "x".repeat(12_000);
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    let responses = vec![
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        // run 2: the summarize slot serves garbage
        tool_call_with_usage("call_c", &echo_args, 120_000),
        "this is not json".to_string(),
        text_response("ok"),
        // run 3: still un-compacted, the trigger retries and fails again
        tool_call_with_usage("call_c", &echo_args, 120_000),
        "still not json".to_string(),
        text_response("ok"),
    ];
    let server = FakeServer::spawn(responses);

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

    let mut events = Vec::new();
    for i in 0..4 {
        let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
        let result = agent.run(format!("run-{i} {big}"), &mut tx).await;
        assert!(result.is_ok(), "run {i} failed: {:?}", result.err());
        drop(tx);
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
    }

    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 10);
    // Un-compacted: the final request replays everything.
    let replayed = requests[9]["messages"].to_string();
    assert!(replayed.contains("run-0"));
    assert!(!replayed.contains("SUMMARY"));
    assert!(
        agent
            .entries()
            .iter()
            .all(|e| !matches!(e, Entry::Compaction { .. }))
    );

    let values = event_values(&events);
    let failed = values
        .iter()
        .filter(|v| {
            v["type"] == "warned" && v["message"].as_str().unwrap().contains("compaction failed")
        })
        .count();
    assert_eq!(failed, 2);
}

/// A single-span session past the trigger line — all mass between one
/// user message and its answer, so no interior boundary exists — warns
/// ONCE per run that compaction cannot cut yet, instead of silently
/// requesting past the configured window every batch.
#[tokio::test]
async fn trigger_without_a_cut_point_warns_once_per_run() {
    let big = "x".repeat(4_000);
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    // Three small tool batches in ONE run: each crosses the 114,688-token
    // trigger line, while total mass stays under KEEP_RECENT (20k).
    let responses = vec![
        tool_call_with_usage("call_c", &echo_args, 120_000),
        tool_call_with_usage("call_c", &echo_args, 120_000),
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
    ];
    let server = FakeServer::spawn(responses);

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
    agent.run(format!("run-0 {big}"), &mut tx).await.unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let values = event_values(&events);
    let warned = values
        .iter()
        .filter(|v| {
            v["type"] == "warned"
                && v["message"]
                    .as_str()
                    .unwrap()
                    .contains("past the compaction line")
        })
        .count();
    assert_eq!(
        warned, 1,
        "one warning per run, not one per batch: {values:?}"
    );
    // And nothing compacted: no boundary ever existed.
    assert!(
        agent
            .entries()
            .iter()
            .all(|e| !matches!(e, Entry::Compaction { .. }))
    );
}

/// The compaction the loop runs lands in a REAL session file: the
/// `Compaction` entry persists, a fresh load of the file carries it, and
/// the replayed wire view from the loaded entries is the summarized one.
#[tokio::test]
async fn loop_compaction_persists_and_replays_from_the_file() {
    let big = "x".repeat(12_000);
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    let responses = vec![
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("ok"),
        tool_call_with_usage("call_c", &echo_args, 120_000),
        text_response("SUMMARY-PERSIST"),
        text_response("ok"),
    ];
    let server = FakeServer::spawn(responses);

    let mut registry = Registry::new();
    registry.register(Box::new(Echo));
    let client = LlmClient::new(&server.base_url).unwrap();
    let root = std::env::temp_dir().join(format!("gg-compact-persist-{}", std::process::id()));
    let store =
        crate::agent::session::io::SessionStore::create_under(&root, &root, "fake-model").unwrap();
    let path = store.path().to_path_buf();
    let mut agent = Agent::new(
        client,
        registry,
        "fake-model".into(),
        512,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        Some(store),
        false,
        131_072,
        Default::default(),
    );

    for i in 0..3 {
        let (mut run_tx, mut run_rx) = tokio::sync::mpsc::channel(128);
        let result = agent.run(format!("run-{i} {big}"), &mut run_tx).await;
        assert!(result.is_ok(), "run {i} failed: {:?}", result.err());
        drop(run_tx);
        while run_rx.recv().await.is_some() {}
    }

    // The file on disk: header + raw entries + the Compaction entry.
    let (_header, loaded, warnings) = crate::agent::session::io::load(&path).unwrap();
    assert!(warnings.is_empty(), "corrupt lines: {warnings:?}");
    let compactions = loaded
        .iter()
        .filter(|e| matches!(e, Entry::Compaction { .. }))
        .count();
    assert_eq!(compactions, 1, "the compaction entry persisted");
    assert!(
        loaded.len() > 8,
        "raw entries stay in the file alongside the compaction"
    );

    // And replaying from the LOADED entries is the summarized view.
    let replayed = crate::agent::session::to_wire(&loaded)
        .iter()
        .map(|m| m.content.as_deref().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(replayed.contains("SUMMARY-PERSIST"));
    assert!(replayed.contains("run-2"));
    assert!(
        !replayed.contains("run-0"),
        "dropped span is summarized away"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn single_run_multi_turn_session_compacts_at_assistant_boundary() {
    let big = "x".repeat(24_000); // ~6k tokens per message
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    let responses = vec![
        tool_call_with_usage("call_1", &echo_args, 120_000),
        tool_call_with_usage("call_2", &echo_args, 120_000),
        text_response("SUMMARY-SINGLE-RUN"),
        text_response("done"),
    ];
    let server = FakeServer::spawn(responses);
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
    agent
        .run("original-prompt-keep-me".into(), &mut tx)
        .await
        .unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    assert!(
        agent
            .entries()
            .iter()
            .any(|e| matches!(e, Entry::Compaction { .. })),
        "single-run session should compact at an assistant boundary"
    );
    let requests = server.requests.lock().unwrap().clone();
    let final_msgs = requests.last().unwrap()["messages"].to_string();
    assert!(final_msgs.contains("SUMMARY-SINGLE-RUN"));
    assert!(final_msgs.contains("[original user request]"));
    assert!(final_msgs.contains("original-prompt-keep-me"));
}

/// A 32K window scales both knobs: 8,192 reserved, a 24,576 compaction
/// line, and 12,288 tokens kept. Two ~9k-token batches cross the kept
/// budget, so compaction drops the first batch and replays the summary
/// plus the second.
#[tokio::test]
async fn tiny_32k_window_scales_reserve_and_keep_recent() {
    let big = "x".repeat(12_000);
    let echo_args = format!(r#"{{"text":"{big}"}}"#);
    let responses = vec![
        tool_call_with_usage("call_1", &echo_args, 30_000),
        text_response("ok"),
        tool_call_with_usage("call_2", &echo_args, 30_000),
        text_response("SUMMARY-32K"),
        text_response("done"),
    ];
    let server = FakeServer::spawn(responses);

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
        32_768,
        Default::default(),
    );

    for run in ["first", "second"] {
        let (mut tx, mut rx) = tokio::sync::mpsc::channel(128);
        let result = agent.run(format!("{run} {big}"), &mut tx).await;
        assert!(result.is_ok(), "{run} run failed: {:?}", result.err());
        drop(tx);
        while rx.recv().await.is_some() {}
    }

    // run 0: tool call + answer; run 1: tool call + summarize + answer.
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    assert!(
        requests[3]["messages"].to_string().contains("first"),
        "the summarize call covers the dropped first batch"
    );
    let replayed = requests[4]["messages"].to_string();
    assert!(replayed.contains("SUMMARY-32K"));
    assert!(replayed.contains("second"));
    assert!(
        !replayed.contains("first xxx"),
        "the dropped first batch is summarized away: {replayed}"
    );

    let compactions: Vec<&Entry> = agent
        .entries()
        .iter()
        .filter(|e| matches!(e, Entry::Compaction { .. }))
        .collect();
    assert_eq!(compactions.len(), 1, "one compaction at the scaled budget");
    let Entry::Compaction { first_kept_id, .. } = compactions[0] else {
        unreachable!()
    };
    assert!(
        agent.entries().iter().any(|e| matches!(
            e,
            Entry::Message { id, message, .. }
                if id == first_kept_id && message.role == Role::User
        )),
        "the boundary names the second user turn"
    );
}
