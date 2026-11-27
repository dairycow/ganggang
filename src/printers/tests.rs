//! Tests for the print-mode renderer: every event's lines, the spinner
//! that overwrites in place, the answer a continued turn accumulates, the
//! per-turn telemetry and the run summary.
use super::*;
use serde_json::json;

fn render(p: &mut Printer, events: Vec<AgentEvent>) -> Vec<String> {
    events.into_iter().flat_map(|e| p.event(e)).collect()
}

#[test]
fn plain_answer_progress_to_stderr_answer_at_end() {
    let mut p = Printer::new();
    let lines = render(
        &mut p,
        vec![
            AgentEvent::RunStarted,
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "0.1.0".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::RunEnded { turns: 1 },
        ],
    );

    // The turn marker, one raw spinner update for the text, the close
    // marker, and the final run summary.
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "── turn 1 ──");
    assert_eq!(
        lines[1], "\rgg streaming… 5 chars",
        "the name is up on the first frame"
    );
    assert_eq!(lines[2], "\r\u{1b}[2K\n");
    assert_eq!(lines[3], "── done: 1 turn(s) · 0 tool(s) ──");
    // Nothing renders to stdout mid-run; the answer is pulled once
    // at the end.
    assert_eq!(p.answer(), "0.1.0");
}

/// Consecutive deltas each overwrite the spinner line in place (raw
/// '\r' lines, no newline of their own); the next non-delta event
/// closes it exactly once, and the lines after the close are clean.
#[test]
fn streamed_deltas_overwrite_in_place_and_close_once() {
    let mut p = Printer::new();
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "foo ".into(),
            },
            AgentEvent::AssistantText {
                delta: "bar".into(),
            },
            AgentEvent::ToolCallStarted {
                name: "read".into(),
                id: "c1".into(),
                args: json!({"path": "Cargo.toml"}),
            },
        ],
    );

    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "── turn 1 ──");
    assert_eq!(lines[1], "\rgg streaming… 4 chars");
    assert_eq!(
        lines[2], "\r  streaming… 7 chars",
        "the second frame is the blank one, and the word keeps its column"
    );
    assert_eq!(lines[3], "\r\u{1b}[2K\n");
    assert_eq!(lines[4], "▸ read Cargo.toml");
    // No later line carries spinner debris, and an unterminated
    // streamed turn still yields its partial text as the answer.
    assert_eq!(p.answer(), "foo bar");
}

#[test]
fn tool_run_shows_call_lines_and_the_final_answer() {
    let mut p = Printer::new();
    let lines = render(
        &mut p,
        vec![
            AgentEvent::RunStarted,
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "checking".into(),
            },
            AgentEvent::ToolCallStarted {
                name: "read".into(),
                id: "c1".into(),
                args: json!({"path": "Cargo.toml"}),
            },
            AgentEvent::ToolCallFinished {
                name: "read".into(),
                id: "c1".into(),
                output: "…file contents…".into(),
                is_error: false,
            },
            AgentEvent::ToolCallStarted {
                name: "bash".into(),
                id: "c2".into(),
                args: json!({"command": "cargo test"}),
            },
            AgentEvent::ToolCallFinished {
                name: "bash".into(),
                id: "c2".into(),
                output: "error[E0308]: mismatched\ntypes".into(),
                is_error: true,
            },
            AgentEvent::TurnEnded {
                stop_reason: "tool_calls".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "one test fails".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::RunEnded { turns: 2 },
        ],
    );

    let err = lines;
    assert!(err.contains(&"── turn 1 ──".to_string()));
    assert!(err.contains(&"── turn 2 ──".to_string()));
    assert_eq!(
        err[1], "\rgg streaming… 8 chars",
        "the narration streams a spinner line"
    );
    assert_eq!(
        err[2], "\r\u{1b}[2K\n",
        "the spinner closes before the tool line"
    );
    assert_eq!(err[3], "▸ read Cargo.toml", "tool start shows compact args");
    assert_eq!(err[4], "✓ read");
    assert_eq!(
        err[6], "✗ bash — error[E0308]: mismatched types",
        "error finish carries a one-line excerpt"
    );
    // The answer is the FINAL turn's text, not the early narration.
    assert_eq!(p.answer(), "one test fails");
}

#[test]
fn warnings_render_prefixed() {
    let mut p = Printer::new();
    let lines = render(
        &mut p,
        vec![AgentEvent::Warned {
            message: "response truncated".into(),
        }],
    );
    assert_eq!(lines, vec!["warning: response truncated"]);
}

/// A continued answer (cut stream or `length`, then the loop asks for
/// the rest) prints as ONE answer: the pieces accumulate instead of
/// the last turn replacing the earlier one.
#[test]
fn a_continued_answer_is_printed_as_one_answer() {
    let mut p = Printer::new();
    render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "half an".into(),
            },
            AgentEvent::Warned {
                message: "stream ended early; continuing from the partial answer (1/3)".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stream_cut".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "swer".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::RunEnded { turns: 2 },
        ],
    );
    assert_eq!(p.answer(), "half answer");
}

#[test]
fn aborted_run_still_yields_the_last_text() {
    let mut p = Printer::new();
    render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "partial work".into(),
            },
            AgentEvent::Warned {
                message: "run aborted".into(),
            },
        ],
    );
    assert_eq!(p.answer(), "partial work");
}

/// Print mode omits thinking entirely (§15, M7.3) — no marker, no
/// line — and a thinking fragment keeps the spinner line alive the
/// way a text delta does (progress is progress; the spinner should
/// not flap while the model works through its reasoning).
#[test]
fn thinking_renders_nothing_and_keeps_the_spinner_alive() {
    let mut p = Printer::new();
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "ans".into(),
            },
            AgentEvent::Thinking {
                delta: "internally reasoning".into(),
            },
            AgentEvent::Thinking {
                delta: " more".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
        ],
    );
    // Turn marker, one spinner update (the text), then the close on
    // TurnEnded — the thinking events contributed nothing.
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "── turn 1 ──");
    assert_eq!(lines[1], "\rgg streaming… 3 chars");
    assert_eq!(lines[2], "\r\u{1b}[2K\n");
    // Thinking never reaches the answer either.
    assert_eq!(p.answer(), "ans");
}

/// Print mode spins the footer's frames, so the two surfaces say the same
/// thing while a run works: the name flashes on, off, on, one frame per
/// text delta, and the word after it keeps its column on the blank frame.
#[test]
fn the_spinner_flashes_the_footer_s_mark() {
    let delta = |text: &str| AgentEvent::AssistantText { delta: text.into() };
    let mut p = Printer::new();
    let lines = render(&mut p, vec![delta("a"), delta("b"), delta("c"), delta("d")]);
    assert_eq!(
        lines,
        vec![
            "\rgg streaming… 1 chars",
            "\r  streaming… 2 chars",
            "\rgg streaming… 3 chars",
            "\rgg streaming… 4 chars",
        ]
    );
    assert_eq!(p.answer(), "abcd");
}

/// No TTY on stderr ⇒ deltas render nothing at all: no spinner, no
/// ANSI, nothing for a redirected stream to choke on. Other progress
/// lines are unaffected.
#[test]
fn quiet_spinner_emits_nothing_for_deltas() {
    let mut p = Printer::new().with_spinner(false);
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "hello ".into(),
            },
            AgentEvent::AssistantText {
                delta: "world".into(),
            },
            AgentEvent::ToolCallStarted {
                name: "read".into(),
                id: "c1".into(),
                args: json!({"path": "Cargo.toml"}),
            },
        ],
    );
    assert_eq!(
        lines,
        vec!["── turn 1 ──".to_string(), "▸ read Cargo.toml".to_string()]
    );
    // No spinner was ever open, so closing emits nothing either.
    assert!(p.close().is_empty());
    assert_eq!(p.answer(), "hello world");
}

/// A run that dies mid-stream ends the channel with the spinner
/// still open: `close` erases it once, and is a no-op after that.
#[test]
fn close_erases_a_spinner_left_open_by_a_dead_run() {
    let mut p = Printer::new();
    render(
        &mut p,
        vec![AgentEvent::AssistantText {
            delta: "half an answ".into(),
        }],
    );
    assert_eq!(p.close(), vec!["\r\u{1b}[2K\n".to_string()]);
    assert!(p.close().is_empty());
}

#[test]
fn smart_tool_formatting_and_turn_telemetry_render_cleanly() {
    let mut p = Printer::new().with_spinner(false);
    let cmd = format!("cargo test {}", "a".repeat(120));
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::ToolCallStarted {
                name: "bash".into(),
                id: "c1".into(),
                args: json!({"command": cmd}),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: Some(crate::agent::session::Usage {
                    prompt_tokens: 1200,
                    completion_tokens: 80,
                    total_tokens: 1280,
                    reasoning_tokens: Some(40),
                    ..Default::default()
                }),
                timings: Some(crate::llm::types::Timings {
                    predicted_per_second: Some(62.4),
                    ..Default::default()
                }),
                thinking_pct: Some(50),
            },
            AgentEvent::RunEnded { turns: 1 },
        ],
    );
    assert!(lines[1].starts_with("▸ bash cargo test "));
    assert!(lines[1].contains(&"a".repeat(100)));
    assert_eq!(
        lines[2],
        "  ctx 1200 tok · gen 80 tok · 62.4 tok/s · think 50%"
    );
    assert_eq!(
        lines[3],
        "── done: 1 turn(s) · 1 tool(s) · peak ctx 1200 tok · gen 80 tok (62.4 tok/s) · think 50% ──"
    );
}

#[test]
fn aborted_summary_and_malformed_tool_args_render_cleanly() {
    let mut p = Printer::new().with_spinner(false);
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::ToolCallStarted {
                name: "bash".into(),
                id: "c1".into(),
                args: json!({"unexpected": 42}),
            },
            AgentEvent::ToolCallFinished {
                name: "bash".into(),
                id: "c1".into(),
                output: "bad".into(),
                is_error: true,
            },
            AgentEvent::TurnEnded {
                stop_reason: "tool_calls".into(),
                usage: Some(crate::agent::session::Usage {
                    prompt_tokens: 500,
                    completion_tokens: 100,
                    total_tokens: 600,
                    reasoning_tokens: Some(20),
                    ..Default::default()
                }),
                timings: Some(crate::llm::types::Timings {
                    predicted_n: Some(100),
                    predicted_ms: Some(2000.0),
                    ..Default::default()
                }),
                thinking_pct: Some(20),
            },
            AgentEvent::RunAborted,
        ],
    );
    assert_eq!(lines[1], r#"▸ bash {"unexpected":42}"#);
    assert_eq!(
        lines.last().unwrap(),
        "── aborted after 1 turn(s) · 1 tool(s) (1 err) · peak ctx 500 tok · gen 100 tok (50.0 tok/s) · think 20% ──"
    );
}

/// The run summary carries the session's cache hit rate once the
/// provider reports cache reads: `cache NN%` between the peak context
/// and the generation figures, over the whole run's prompt total.
#[test]
fn the_run_summary_carries_the_cache_hit_rate() {
    let turn = |cache_read: u32| AgentEvent::TurnEnded {
        stop_reason: "stop".into(),
        usage: Some(crate::agent::session::Usage {
            prompt_tokens: 1000,
            completion_tokens: 50,
            total_tokens: 1050,
            cache_read_tokens: cache_read,
            ..Default::default()
        }),
        timings: None,
        thinking_pct: None,
    };
    let mut p = Printer::new().with_spinner(false);
    let lines = render(
        &mut p,
        vec![
            AgentEvent::TurnStarted,
            turn(800),
            AgentEvent::TurnStarted,
            turn(950),
            AgentEvent::RunEnded { turns: 2 },
        ],
    );
    // 1750 of the 2000 prompt tokens came from the cache.
    assert_eq!(
        lines.last().unwrap(),
        "── done: 2 turn(s) · 0 tool(s) · peak ctx 1000 tok · cache 87% · gen 100 tok ──"
    );
}
