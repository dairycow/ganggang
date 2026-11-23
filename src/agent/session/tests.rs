//! Tests for [`super`]: session round-trips and domain ↔ wire conversion.

use super::*;
use crate::llm::types::Choice;

#[test]
fn session_minimal_round_trips_entry_by_entry() {
    let raw = include_str!("../../../fixtures/session_minimal.jsonl");
    assert!(raw.lines().count() >= 4);
    for line in raw.lines() {
        let entry: Entry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("fixture line failed to parse ({e}): {line}"));
        let back = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(line).unwrap(),
            serde_json::from_str::<Value>(&back).unwrap(),
            "round trip changed: {line}"
        );
    }
}

#[test]
fn roles_serialize_snake_case() {
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), r#""user""#);
    assert_eq!(
        serde_json::to_string(&Role::Assistant).unwrap(),
        r#""assistant""#
    );
    assert_eq!(
        serde_json::to_string(&Role::ToolResult).unwrap(),
        r#""tool_result""#
    );
}

#[test]
fn entry_ids_are_8_hex() {
    let id = new_entry_id();
    assert_eq!(id.len(), 8);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn tool_call_fixture_replays_through_domain() {
    let raw = include_str!("../../../fixtures/tool_call_response.json");
    let resp: ChatResponse = serde_json::from_str(raw).unwrap();
    let (msg, warnings) = from_response(resp).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.content, None);

    let usage = msg.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 305);
    assert_eq!(usage.completion_tokens, 81);

    let tc = &msg.tool_calls[0];
    assert_eq!(tc.name, "calculator");
    assert_eq!(tc.args["expression"], "12*9");

    let entry = Entry::Message {
        id: new_entry_id(),
        parent_id: None,
        timestamp: now_iso(),
        message: msg,
    };
    let wire = to_wire(&[entry]);
    assert_eq!(wire.len(), 1);

    // The M1 replay guarantee, now through the domain: arguments go back
    // out as a JSON string and the call id survives.
    let wire_tool = &wire[0].tool_calls[0];
    assert_eq!(wire_tool.id, "call_33948a7d7d2a4335a9490a2e");
    assert_eq!(wire_tool.function.name, "calculator");
    let reargs: Value = serde_json::from_str(&wire_tool.function.arguments).unwrap();
    assert_eq!(reargs["expression"], "12*9");
}

#[test]
fn malformed_arguments_degrade_to_empty_with_warning() {
    let resp = ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".into(),
                tool_calls: vec![WireToolCall::function(
                    "call_1".into(),
                    "read".into(),
                    "{not json".into(),
                )],
                ..Default::default()
            },
            finish_reason: None,
        }],
        usage: None,
    };
    let (msg, warnings) = from_response(resp).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not valid JSON"));
    assert_eq!(msg.tool_calls[0].args, serde_json::json!({}));
    assert!(msg.tool_calls[0].args_error.is_some());
}

/// A call whose arguments failed to parse still replays: the assistant
/// message keeps the call (its tool_result references the id — dropping
/// either orphans the other and the next request gets rejected), and the
/// arguments go back out as an empty object rather than the invalid text.
#[test]
fn unparseable_args_replay_as_empty_object_and_never_drop_the_call() {
    let resp = ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".into(),
                tool_calls: vec![WireToolCall::function(
                    "call_bad".into(),
                    "bash".into(),
                    "{not json".into(),
                )],
                ..Default::default()
            },
            finish_reason: None,
        }],
        usage: None,
    };
    let (msg, _) = from_response(resp).unwrap();
    let entries = [Entry::Message {
        id: new_entry_id(),
        parent_id: None,
        timestamp: now_iso(),
        message: msg,
    }];

    let wire = to_wire(&entries);
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].tool_calls.len(), 1);
    assert_eq!(wire[0].tool_calls[0].id, "call_bad");
    assert_eq!(wire[0].tool_calls[0].function.arguments, "{}");
}

#[test]
fn empty_arguments_are_an_empty_object_not_a_warning() {
    let resp = ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".into(),
                tool_calls: vec![WireToolCall::function(
                    "call_1".into(),
                    "bash".into(),
                    String::new(),
                )],
                ..Default::default()
            },
            finish_reason: None,
        }],
        usage: None,
    };
    let (msg, warnings) = from_response(resp).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(msg.tool_calls[0].args, serde_json::json!({}));
}

#[test]
fn no_choices_is_an_error() {
    let resp = ChatResponse {
        choices: vec![],
        usage: None,
    };
    assert!(from_response(resp).is_err());
}

#[test]
fn roles_map_to_wire_names_and_compaction_governs_replay() {
    let raw = include_str!("../../../fixtures/session_minimal.jsonl");
    let entries: Vec<Entry> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // Without the compaction entry: plain role mapping.
    let plain = to_wire(&entries[..3]);
    assert_eq!(plain.len(), 3);
    assert_eq!(plain[0].role, "user");
    assert_eq!(plain[1].role, "assistant");
    assert_eq!(plain[1].tool_calls[0].function.name, "read");
    assert_eq!(plain[2].role, "tool");
    assert_eq!(
        plain[2].tool_call_id.as_deref(),
        Some("call_33948a7d7d2a4335")
    );
    assert_eq!(plain[2].content.as_deref(), Some("96 lines"));

    // With it: the LAST compaction governs (§14) — its summary leads as
    // a user note and replay continues from `firstKeptId`. The fixture's
    // boundary names a tool_result (a hand-made file): replay snaps
    // forward to the next user message, and there is none after it, so
    // the note carries the context alone — an orphaned tool_result is
    // never sent.
    let wire = to_wire(&entries);
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].role, "user");
    let content = wire[0].content.as_deref().unwrap();
    assert!(content.contains("[conversation before this point was compacted]"));
    assert!(content.contains("fix the failing test"));
}

/// A compaction whose boundary names a real user message replays the
/// kept span whole, preceded by the summary note.
#[test]
fn compaction_boundary_on_a_user_message_replays_from_there() {
    let raw = include_str!("../../../fixtures/session_minimal.jsonl");
    let mut entries: Vec<Entry> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let Entry::Compaction {
        ref mut first_kept_id,
        ..
    } = entries[3]
    else {
        panic!("fixture changed");
    };
    *first_kept_id = "a1b2c3d4".to_string(); // the fixture's user entry

    let wire = to_wire(&entries);
    assert_eq!(wire.len(), 4);
    assert_eq!(wire[0].role, "user");
    assert!(
        wire[0]
            .content
            .as_deref()
            .unwrap()
            .contains("[conversation before this point was compacted]")
    );
    assert_eq!(wire[1].role, "user");
    assert_eq!(wire[1].content.as_deref(), Some("fix the failing test"));
    assert_eq!(wire[2].role, "assistant");
    assert_eq!(wire[3].role, "tool");
}

/// Thinking (M7.3): `from_response` captures `reasoning_content` onto the
/// assistant message (empty string normalizes to `None`), the session
/// line persists it only when non-empty, and `to_wire` never sends it
/// back to the model.
#[test]
fn reasoning_is_captured_persisted_minimally_and_never_replayed() {
    let resp: ChatResponse = serde_json::from_value(serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "42",
                "reasoning_content": "the user wants a number",
            },
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    }))
    .unwrap();
    let (msg, _) = from_response(resp).unwrap();
    assert_eq!(msg.reasoning.as_deref(), Some("the user wants a number"));

    let entry = Entry::Message {
        id: "e1".into(),
        parent_id: None,
        timestamp: now_iso(),
        message: msg,
    };
    let line = serde_json::to_string(&entry).unwrap();
    assert!(line.contains("\"reasoning\":\"the user wants a number\""));
    // Round-trips back through the domain.
    let back: Entry = serde_json::from_str(&line).unwrap();
    let Entry::Message { message, .. } = &back else {
        panic!("fixture changed");
    };
    assert_eq!(
        message.reasoning.as_deref(),
        Some("the user wants a number")
    );

    // Replay keeps thinking to the human.
    for wire in to_wire(&[entry]) {
        assert_eq!(wire.reasoning_content, None);
    }

    // Minimal write: absent or empty reasoning stays absent on the line.
    let resp: ChatResponse = serde_json::from_value(serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "hi", "reasoning_content": ""},
            "finish_reason": "stop",
        }],
    }))
    .unwrap();
    let (msg, _) = from_response(resp).unwrap();
    assert_eq!(msg.reasoning, None, "empty reasoning normalizes to None");
    let entry = Entry::Message {
        id: "e2".into(),
        parent_id: None,
        timestamp: now_iso(),
        message: msg,
    };
    let line = serde_json::to_string(&entry).unwrap();
    assert!(!line.contains("reasoning"));
}

#[test]
fn to_wire_with_replay_reasoning_populates_assistant_reasoning_content() {
    let entries = vec![
        Entry::Message {
            id: "e1".into(),
            parent_id: None,
            timestamp: now_iso(),
            message: Message {
                role: Role::Assistant,
                content: Some("hi".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: Some("thought".into()),
            },
        },
        Entry::Message {
            id: "e2".into(),
            parent_id: Some("e1".into()),
            timestamp: now_iso(),
            message: Message {
                role: Role::Assistant,
                content: Some("no thought".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: None,
            },
        },
    ];
    let plain = to_wire(&entries);
    assert_eq!(plain[0].reasoning_content, None);
    assert_eq!(plain[1].reasoning_content, None);

    let replayed = to_wire_with(&entries, true);
    assert_eq!(replayed[0].reasoning_content.as_deref(), Some("thought"));
    assert_eq!(replayed[1].reasoning_content.as_deref(), Some(""));
}
