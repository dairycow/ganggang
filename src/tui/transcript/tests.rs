//! Tests for [`super`]: the event-stream fold, collapsed shapes, expand
//! cycling, and the markers — all without a terminal.

use serde_json::json;

use super::{LineKind, Transcript};

fn texts(t: &Transcript) -> Vec<String> {
    t.render().into_iter().map(|l| l.text).collect()
}

fn kinds(t: &Transcript) -> Vec<LineKind> {
    t.render().into_iter().map(|l| l.kind).collect()
}

fn script(events: &[crate::events::AgentEvent]) -> Transcript {
    let mut t = Transcript::default();
    for event in events {
        t.apply(event);
    }
    t
}

use crate::events::AgentEvent;

/// The M2.9 plain-answer script: one user line, one assistant answer.
#[test]
fn plain_answer_renders_user_then_assistant() {
    let mut t = Transcript::default();
    t.push_user("what version?");
    t.apply(&AgentEvent::RunStarted);
    t.apply(&AgentEvent::TurnStarted);
    t.apply(&AgentEvent::AssistantText {
        delta: "0.1.0".into(),
    });
    t.apply(&AgentEvent::TurnEnded {
        stop_reason: "stop".into(),
        usage: None,
        timings: None,
        thinking_pct: None,
    });
    t.apply(&AgentEvent::RunEnded { turns: 1 });

    assert_eq!(texts(&t), vec!["you │ what version?", "0.1.0"]);
    assert_eq!(kinds(&t), vec![LineKind::User, LineKind::Assistant]);
}

/// The tool round-trip script with an error call: collapsed one-liners
/// with status glyphs, dim warnings, no raw output on the collapsed line.
#[test]
fn tool_round_trip_collapses_calls_and_marks_errors() {
    let mut t = Transcript::default();
    t.push_user("fix the test");
    t.apply(&AgentEvent::TurnStarted);
    t.apply(&AgentEvent::AssistantText {
        delta: "checking".into(),
    });
    t.apply(&AgentEvent::ToolCallStarted {
        name: "read".into(),
        id: "c1".into(),
        args: json!({"path": "Cargo.toml"}),
    });
    t.apply(&AgentEvent::ToolCallFinished {
        name: "read".into(),
        id: "c1".into(),
        output: "[package]\nname = \"gg\"".into(),
        is_error: false,
    });
    t.apply(&AgentEvent::ToolCallStarted {
        name: "bash".into(),
        id: "c2".into(),
        args: json!({"command": "cargo test"}),
    });
    t.apply(&AgentEvent::ToolCallFinished {
        name: "bash".into(),
        id: "c2".into(),
        output: "error[E0308]: mismatched types".into(),
        is_error: true,
    });
    t.apply(&AgentEvent::TurnEnded {
        stop_reason: "tool_calls".into(),
        usage: None,
        timings: None,
        thinking_pct: None,
    });
    t.apply(&AgentEvent::TurnStarted);
    t.apply(&AgentEvent::AssistantText {
        delta: "one test fails".into(),
    });

    assert_eq!(
        texts(&t),
        vec![
            "you │ fix the test",
            "checking",
            "▸ read {\"path\":\"Cargo.toml\"} ✓",
            "▸ bash {\"command\":\"cargo test\"} ✗",
            "one test fails",
        ]
    );
    let k = kinds(&t);
    assert_eq!(k[2], LineKind::Tool);
    assert_eq!(k[3], LineKind::ToolError);
}

/// A running call (started, not finished) shows no status glyph yet.
#[test]
fn an_unfinished_call_shows_no_status() {
    let t = script(&[AgentEvent::ToolCallStarted {
        name: "bash".into(),
        id: "c1".into(),
        args: json!({"command": "sleep 5"}),
    }]);
    assert_eq!(texts(&t), vec!["▸ bash {\"command\":\"sleep 5\"}"]);
}

/// The abort script: warnings and the marker render dim.
#[test]
fn abort_renders_dim_warning_and_marker() {
    let mut t = Transcript::default();
    t.push_user("two things");
    t.apply(&AgentEvent::ToolCallStarted {
        name: "echo".into(),
        id: "c1".into(),
        args: json!({"text": "first"}),
    });
    t.apply(&AgentEvent::ToolCallFinished {
        name: "echo".into(),
        id: "c1".into(),
        output: "first".into(),
        is_error: false,
    });
    t.apply(&AgentEvent::Warned {
        message: "run aborted".into(),
    });
    t.apply(&AgentEvent::RunAborted);

    assert_eq!(
        texts(&t),
        vec![
            "you │ two things",
            "▸ echo {\"text\":\"first\"} ✓",
            "⚠ run aborted",
            "■ run aborted",
        ]
    );
    assert_eq!(kinds(&t)[2], LineKind::Warning, "a warning is a warning");
    assert_eq!(
        kinds(&t)[3],
        LineKind::Aborted,
        "the marker is its own kind"
    );
}

/// Consecutive deltas continue one assistant block; a turn boundary
/// starts a new one (the fold tracks the boundary, not just line shape).
#[test]
fn deltas_append_within_a_turn_and_split_across_turns() {
    let mut t = Transcript::default();
    t.apply(&AgentEvent::AssistantText {
        delta: "part one ".into(),
    });
    t.apply(&AgentEvent::AssistantText {
        delta: "part two".into(),
    });
    t.apply(&AgentEvent::TurnStarted);
    t.apply(&AgentEvent::AssistantText {
        delta: "new turn".into(),
    });
    assert_eq!(texts(&t), vec!["part one part two", "new turn"]);
}

/// Ctrl+O expands the LAST collapsed call first, then — with all
/// expanded — collapses everything.
#[test]
fn toggle_expand_cycles_last_collapsed_then_collapses_all() {
    let mut t = script(&[
        AgentEvent::ToolCallStarted {
            name: "read".into(),
            id: "c1".into(),
            args: json!({"path": "a"}),
        },
        AgentEvent::ToolCallFinished {
            name: "read".into(),
            id: "c1".into(),
            output: "first".into(),
            is_error: false,
        },
        AgentEvent::ToolCallStarted {
            name: "read".into(),
            id: "c2".into(),
            args: json!({"path": "b"}),
        },
        AgentEvent::ToolCallFinished {
            name: "read".into(),
            id: "c2".into(),
            output: "second".into(),
            is_error: false,
        },
    ]);

    t.toggle_expand(); // last collapsed = c2
    assert_eq!(
        texts(&t),
        vec![
            "▸ read {\"path\":\"a\"} ✓",
            "▾ read {\"path\":\"b\"} ✓",
            "  second"
        ]
    );

    t.toggle_expand(); // now c1
    assert_eq!(
        texts(&t),
        vec![
            "▾ read {\"path\":\"a\"} ✓",
            "  first",
            "▾ read {\"path\":\"b\"} ✓",
            "  second"
        ]
    );

    t.toggle_expand(); // all expanded → collapse all
    assert_eq!(
        texts(&t),
        vec!["▸ read {\"path\":\"a\"} ✓", "▸ read {\"path\":\"b\"} ✓"]
    );
}

/// Expanded output is capped at 30 lines with a visible marker.
#[test]
fn expanded_output_caps_at_30_lines_with_a_marker() {
    let big: String = (0..45)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .trim_end()
        .to_string();
    let mut t = script(&[
        AgentEvent::ToolCallStarted {
            name: "bash".into(),
            id: "c1".into(),
            args: json!({"command": "seq"}),
        },
        AgentEvent::ToolCallFinished {
            name: "bash".into(),
            id: "c1".into(),
            output: big,
            is_error: false,
        },
    ]);
    t.toggle_expand();
    let lines = texts(&t);
    assert_eq!(lines.len(), 32, "header + 30 + marker");
    assert_eq!(lines[31], "  [truncated: 45 lines]");
    assert_eq!(lines[1], "  line 0");
    assert_eq!(lines[30], "  line 29");
}

/// Long args collapse to one ~60-char line with an ellipsis.
#[test]
fn long_args_collapse_with_an_ellipsis() {
    let long = "x".repeat(200);
    let t = script(&[AgentEvent::ToolCallStarted {
        name: "write".into(),
        id: "c1".into(),
        args: json!({"content": long}),
    }]);
    let rendered = texts(&t)[0].clone();
    assert!(rendered.starts_with("▸ write {\"content\":\"xxxx"));
    assert!(rendered.ends_with('…'));
    assert!(rendered.chars().count() < 80, "{rendered}");
}

#[test]
fn shell_blocks_render_with_exit_code_and_context_marker() {
    let mut t = Transcript::default();
    t.push_shell("echo hi", "hi\n", Some(0), true);
    t.push_shell("false", "boom\n", Some(1), false);

    let lines = t.render();
    assert!(
        lines
            .iter()
            .any(|l| l.text == "$ echo hi → exit 0" && l.kind == LineKind::Tool)
    );
    assert!(lines.iter().any(|l| l.text == "  hi"));
    assert!(
        lines
            .iter()
            .any(|l| l.text.starts_with("$ false → exit 1")
                && l.text.ends_with("(not sent to model)"))
    );
}

#[test]
fn shell_output_caps_at_twenty_lines_with_a_marker() {
    let output = (0..40)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let mut t = Transcript::default();
    t.push_shell("seq", &output, Some(0), true);

    let rendered = t
        .render()
        .into_iter()
        .filter(|l| l.kind == LineKind::Output)
        .count();
    assert!(
        rendered <= 21,
        "20 output lines + truncation marker, got {rendered}"
    );
    assert!(
        t.render()
            .iter()
            .any(|l| l.text.contains("[truncated: 40 lines]"))
    );
}

#[test]
fn last_answer_is_the_most_recent_assistant_text() {
    let mut t = Transcript::default();
    assert_eq!(t.last_answer(), None);
    t.apply(&AgentEvent::AssistantText {
        delta: "first".into(),
    });
    t.apply(&AgentEvent::ToolCallStarted {
        name: "read".into(),
        id: "c1".into(),
        args: serde_json::json!({}),
    });
    t.apply(&AgentEvent::AssistantText {
        delta: "second".into(),
    });
    assert_eq!(t.last_answer(), Some("second"));
}

#[test]
fn multi_line_warnings_keep_their_lines_and_alignment() {
    let mut t = Transcript::default();
    t.apply(&AgentEvent::Warned {
        message: "cargo build failed:\nerror[E0425]: nope\n  --> src/main.rs:3".into(),
    });
    assert_eq!(
        texts(&t),
        vec![
            "⚠ cargo build failed:",
            "  error[E0425]: nope",
            "    --> src/main.rs:3",
        ]
    );
}

#[test]
fn info_blocks_render_dim_without_a_warning_glyph() {
    let mut t = Transcript::default();
    t.push_info("sessions (newest first):\n  1) today abc12345 — m");
    let lines = t.render();
    assert!(
        lines
            .iter()
            .any(|l| l.text == "sessions (newest first):" && l.kind == LineKind::Info)
    );
    assert!(lines.iter().any(|l| l.text.starts_with("  1) today")));
    assert!(!lines.iter().any(|l| l.text.starts_with("⚠")));
}

/// Thinking (M7.3): hidden by default, Ctrl+T shows it dimmed above the
/// turn's text, deltas append within a block, and the cap marks its cut.
#[test]
fn thinking_is_hidden_until_toggled_then_renders_dim_and_capped() {
    use crate::events::AgentEvent;
    let mut t = Transcript::default();
    t.push_user("hi");
    t.apply(&AgentEvent::Thinking {
        delta: "consider".into(),
    });
    t.apply(&AgentEvent::Thinking {
        delta: " the question".into(),
    });
    t.apply(&AgentEvent::AssistantText {
        delta: "hello".into(),
    });

    // Default: invisible, answer intact.
    assert_eq!(texts(&t), vec!["you │ hi", "hello"]);

    t.toggle_thinking();
    assert_eq!(
        texts(&t),
        vec!["you │ hi", "~ consider the question", "hello",]
    );
    assert_eq!(kinds(&t)[1], LineKind::Thinking);

    // And back off again.
    t.toggle_thinking();
    assert_eq!(texts(&t), vec!["you │ hi", "hello"]);

    // The cap: 40 lines of thinking render 30 plus a truncation marker.
    let mut t = Transcript::default();
    t.apply(&AgentEvent::Thinking {
        delta: (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    t.toggle_thinking();
    let rendered = texts(&t);
    assert_eq!(rendered.len(), 31);
    assert_eq!(rendered[0], "~ line 0");
    assert_eq!(rendered[29], "~ line 29");
    assert_eq!(rendered[30], "~ [truncated: 40 lines]");
}

/// A resumed session replays persisted thinking as one block before the
/// text, under the same toggle.
#[test]
fn resume_seeds_thinking_before_the_text() {
    use crate::agent::session::{Entry, Message, Role};
    let assistant = |reasoning: Option<&str>, content: Option<&str>| Entry::Message {
        id: "a".into(),
        parent_id: None,
        timestamp: "t".into(),
        message: Message {
            role: Role::Assistant,
            content: content.map(String::from),
            reasoning: reasoning.map(String::from),
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
            is_error: false,
        },
    };
    let entries = vec![
        Entry::Message {
            id: "u".into(),
            parent_id: None,
            timestamp: "t".into(),
            message: Message {
                role: Role::User,
                content: Some("hi".into()),
                reasoning: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
            },
        },
        assistant(Some("pondered"), Some("hello")),
    ];

    let mut t = Transcript::default();
    t.extend_from_entries(&entries);
    assert_eq!(texts(&t), vec!["you │ hi", "hello"], "hidden by default");
    t.toggle_thinking();
    assert_eq!(texts(&t), vec!["you │ hi", "~ pondered", "hello"]);
}
