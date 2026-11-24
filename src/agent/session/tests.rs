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
        timings: None,
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
        timings: None,
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
        timings: None,
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
        timings: None,
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

    let replayed = to_wire_with(&entries, ReplayReasoning::All);
    assert_eq!(replayed[0].reasoning_content.as_deref(), Some("thought"));
    assert_eq!(replayed[1].reasoning_content.as_deref(), Some(""));
}

#[test]
fn to_wire_with_turn_scoped_replay_keeps_only_current_turn_reasoning() {
    let msg = |role: Role, content: Option<&str>, reasoning: Option<&str>| Message {
        role,
        content: content.map(String::from),
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: None,
        is_error: false,
        reasoning: reasoning.map(String::from),
    };
    let entries = vec![
        Entry::Message {
            id: "u1".into(),
            parent_id: None,
            timestamp: now_iso(),
            message: msg(Role::User, Some("first"), None),
        },
        Entry::Message {
            id: "a1".into(),
            parent_id: Some("u1".into()),
            timestamp: now_iso(),
            message: msg(Role::Assistant, Some("ans1"), Some("old-thought")),
        },
        Entry::Message {
            id: "u2".into(),
            parent_id: Some("a1".into()),
            timestamp: now_iso(),
            message: msg(Role::User, Some("second"), None),
        },
        Entry::Message {
            id: "a2".into(),
            parent_id: Some("u2".into()),
            timestamp: now_iso(),
            message: msg(Role::Assistant, None, Some("turn2-thought-1")),
        },
        Entry::Message {
            id: "a3".into(),
            parent_id: Some("a2".into()),
            timestamp: now_iso(),
            message: msg(Role::Assistant, Some("ans2"), Some("turn2-thought-2")),
        },
    ];
    let wire = to_wire_with(&entries, ReplayReasoning::Turn);
    assert_eq!(
        wire[1].reasoning_content, None,
        "old turn reasoning dropped"
    );
    assert_eq!(
        wire[3].reasoning_content.as_deref(),
        Some("turn2-thought-1")
    );
    assert_eq!(
        wire[4].reasoning_content.as_deref(),
        Some("turn2-thought-2")
    );
}

#[test]
fn compaction_boundary_on_assistant_pins_original_user_request() {
    let entries = vec![
        Entry::Message {
            id: "u1".into(),
            parent_id: None,
            timestamp: now_iso(),
            message: Message {
                role: Role::User,
                content: Some("build feature X".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: None,
            },
        },
        Entry::Message {
            id: "a1".into(),
            parent_id: Some("u1".into()),
            timestamp: now_iso(),
            message: Message {
                role: Role::Assistant,
                content: Some("step 1".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: None,
            },
        },
        Entry::Message {
            id: "a2".into(),
            parent_id: Some("a1".into()),
            timestamp: now_iso(),
            message: Message {
                role: Role::Assistant,
                content: Some("step 2".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: Some("keep-me".into()),
            },
        },
        Entry::Compaction {
            id: "c1".into(),
            parent_id: Some("a2".into()),
            timestamp: now_iso(),
            summary: "did step 1".into(),
            first_kept_id: "a2".into(),
        },
    ];
    let wire = to_wire_with(&entries, ReplayReasoning::Turn);
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[0].role, "user");
    let note = wire[0].content.as_deref().unwrap();
    assert!(note.contains("[original user request]\nbuild feature X"));
    assert!(note.contains("[conversation before this point was compacted]\ndid step 1"));
    assert_eq!(wire[1].role, "assistant");
    assert_eq!(wire[1].reasoning_content.as_deref(), Some("keep-me"));
}

#[test]
fn thinking_pct_prefers_usage_reasoning_tokens_then_char_ratio() {
    let m1 = Message {
        role: Role::Assistant,
        content: Some("ok".into()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 100,
            total_tokens: 110,
            reasoning_tokens: Some(75),
        }),
        is_error: false,
        reasoning: Some("x".into()),
    };
    assert_eq!(thinking_pct(&m1), Some(75));

    let m2 = Message {
        role: Role::Assistant,
        content: Some("1234".into()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: None,
        is_error: false,
        reasoning: Some("123456".into()),
    };
    assert_eq!(thinking_pct(&m2), Some(60));

    let m3 = Message {
        role: Role::Assistant,
        content: Some("1234".into()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 100,
            total_tokens: 110,
            reasoning_tokens: Some(0),
        }),
        is_error: false,
        reasoning: Some("123456".into()),
    };
    assert_eq!(thinking_pct(&m3), Some(60));
}

#[test]
fn notices_ride_the_last_noticeable_entry_across_normal_and_compacted_histories() {
    let msg = |role: Role, content: Option<&str>| Message {
        role,
        content: content.map(String::from),
        tool_calls: Vec::new(),
        tool_call_id: (role == Role::ToolResult).then(|| "call_1".into()),
        usage: None,
        is_error: false,
        reasoning: None,
    };
    let entry = |id: &str, role: Role, content: Option<&str>| Entry::Message {
        id: id.into(),
        parent_id: None,
        timestamp: now_iso(),
        message: msg(role, content),
    };

    // Normal [User, Assistant, ToolResult]: notice rides ToolResult ("t1").
    let normal = vec![
        entry("u1", Role::User, Some("go")),
        entry("a1", Role::Assistant, None),
        entry("t1", Role::ToolResult, Some("ok")),
    ];
    assert_eq!(last_noticeable_entry_id(&normal).as_deref(), Some("t1"));
    let mut notices = HashMap::new();
    notices.insert("t1".into(), "[notice]".into());
    let wire = to_wire_with_notices(&normal, ReplayReasoning::Never, &notices);
    assert_eq!(wire[0].content.as_deref(), Some("go"));
    assert_eq!(wire[1].content, None);
    assert_eq!(wire[2].content.as_deref(), Some("ok\n\n[notice]"));

    // Trailing Compaction with kept = Some("a1"): last wire message is still "t1".
    let mut with_kept = normal.clone();
    with_kept.push(Entry::Compaction {
        id: "c1".into(),
        parent_id: Some("t1".into()),
        timestamp: now_iso(),
        summary: "sum".into(),
        first_kept_id: "a1".into(),
    });
    assert_eq!(last_noticeable_entry_id(&with_kept).as_deref(), Some("t1"));
    let wire = to_wire_with_notices(&with_kept, ReplayReasoning::Never, &notices);
    assert_eq!(
        wire.last().unwrap().content.as_deref(),
        Some("ok\n\n[notice]")
    );

    // Trailing Compaction with kept = None (first_kept_id points to ToolResult):
    // notice rides the compaction note itself ("c1").
    let mut no_kept = normal;
    no_kept.push(Entry::Compaction {
        id: "c1".into(),
        parent_id: Some("t1".into()),
        timestamp: now_iso(),
        summary: "sum".into(),
        first_kept_id: "t1".into(),
    });
    assert_eq!(last_noticeable_entry_id(&no_kept).as_deref(), Some("c1"));
    let mut c_notices = HashMap::new();
    c_notices.insert("c1".into(), "[notice]".into());
    let wire = to_wire_with_notices(&no_kept, ReplayReasoning::Never, &c_notices);
    assert_eq!(wire.len(), 1);
    assert!(
        wire[0]
            .content
            .as_deref()
            .unwrap()
            .ends_with("\n\n[notice]")
    );

    // History ending in Assistant: None.
    let ending_asst = vec![
        entry("u1", Role::User, Some("go")),
        entry("a1", Role::Assistant, Some("done")),
    ];
    assert_eq!(last_noticeable_entry_id(&ending_asst), None);
}
