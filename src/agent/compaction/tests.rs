//! Compaction trigger and token-accounting tests (§14).

use serde_json::json;

use super::*;
use crate::agent::session::{Entry, Message, Role, Usage};

fn msg_entry(id: &str, role: Role, content: &str, usage: Option<Usage>) -> Entry {
    Entry::Message {
        id: id.to_string(),
        parent_id: None,
        timestamp: String::new(),
        message: Message {
            role,
            content: Some(content.to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage,
            is_error: false,
            reasoning: None,
        },
    }
}

/// The trigger is a strict inequality against `window − reserve`, and a
/// window smaller than the reserve saturates to "always past the line"
/// rather than underflowing to "never".
#[test]
fn trigger_fires_past_the_reserved_line() {
    let window = 131_072u32;
    let line = window - RESERVE;
    assert!(
        !should_compact(line, window, None),
        "exactly at the line holds"
    );
    assert!(should_compact(line + 1, window, None), "one past triggers");
    assert!(!should_compact(0, window, None));
    // Degenerate tiny window: reserve exceeds it entirely.
    assert!(should_compact(1, 8_000, None));

    // Threshold clamps below window - RESERVE when smaller, and is
    // clamped by window - RESERVE when larger.
    assert!(!should_compact(65_536, 262_144, Some(65_536)));
    assert!(should_compact(65_537, 262_144, Some(65_536)));
    assert!(!should_compact(window - RESERVE, window, Some(200_000)));
    assert!(should_compact(window - RESERVE + 1, window, Some(200_000)));
}

/// Usage comes from the LAST assistant entry that has one — not the
/// first, and not from user or tool-result entries.
#[test]
fn context_tokens_come_from_the_last_assistant_usage() {
    let entries = vec![
        msg_entry("u1", Role::User, "hello", None),
        msg_entry(
            "a1",
            Role::Assistant,
            "hi",
            Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                ..Usage::default()
            }),
        ),
        msg_entry("t1", Role::ToolResult, "output", None),
        msg_entry(
            "a2",
            Role::Assistant,
            "done",
            Some(Usage {
                prompt_tokens: 300,
                completion_tokens: 40,
                total_tokens: 340,
                ..Usage::default()
            }),
        ),
    ];
    assert_eq!(context_tokens_of(&entries), Some(340));
    assert_eq!(context_tokens_of(&entries[..3]), Some(120));
    assert_eq!(context_tokens_of(&entries[..1]), None);
}

/// The fallback estimate is chars/4 over the wire view's text mass:
/// content plus tool-call names and argument strings.
#[test]
fn wire_estimate_is_chars_over_four_of_text_mass() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("a".repeat(400)),
            ..ChatMessage::default()
        },
        ChatMessage {
            role: "assistant".into(),
            tool_calls: vec![crate::llm::types::ToolCall::function(
                "c1".into(),
                "read".into(),  // 4 chars
                "x".repeat(96), // + 96 = 100
            )],
            ..ChatMessage::default()
        },
    ];
    assert_eq!(estimate_wire_tokens(&messages), 125);
}

/// Per-entry walk estimates measure the entry's own text, never the
/// usage numbers (which double-count the whole request each turn).
#[test]
fn entry_estimates_measure_own_text_only() {
    let e = msg_entry(
        "a1",
        Role::Assistant,
        &"z".repeat(400),
        Some(Usage {
            prompt_tokens: 9_999,
            completion_tokens: 9_999,
            total_tokens: 19_998,
            ..Usage::default()
        }),
    );
    assert_eq!(entry_token_estimate(&e), 100);
    let compacted = Entry::Compaction {
        id: "c9".into(),
        parent_id: None,
        timestamp: String::new(),
        summary: "s".repeat(80),
        first_kept_id: "a1".into(),
    };
    assert_eq!(entry_token_estimate(&compacted), 20);
    assert_eq!(
        serde_json::to_value(&compacted).unwrap()["firstKeptId"],
        json!("a1")
    );
}

// ---------------------------------------------------------- cut point

/// `tokens` are emulated with chars/4 text mass.
fn big(id: &str, role: Role, tokens: u32) -> Entry {
    msg_entry(id, role, &"x".repeat(tokens as usize * 4), None)
}

fn compaction_entry(first_kept_id: &str) -> Entry {
    Entry::Compaction {
        id: "cmp".into(),
        parent_id: None,
        timestamp: String::new(),
        summary: "sum".into(),
        first_kept_id: first_kept_id.into(),
    }
}

/// Nine 3k-token entries: the maximal suffix within KEEP_RECENT opens on
/// the third user message — a clean boundary, no snap needed.
#[test]
fn cut_keeps_the_maximal_suffix_within_budget() {
    let entries = vec![
        big("u1", Role::User, 3_000),
        big("a1", Role::Assistant, 3_000),
        big("t1", Role::ToolResult, 3_000),
        big("u2", Role::User, 3_000),
        big("a2", Role::Assistant, 3_000),
        big("t2", Role::ToolResult, 3_000),
        big("u3", Role::User, 3_000),
        big("a3", Role::Assistant, 3_000),
        big("t3", Role::ToolResult, 3_000),
    ];
    // Suffix sums: 3,6,9,12,15,18, then 21 crosses KEEP_RECENT.
    assert_eq!(cut_point(&entries).as_deref(), Some("u2"));
}

/// When the walk stops mid-span, the cut snaps BACK to that span's user
/// message — span integrity outranks the token budget (pi's rule: never
/// orphan a tool result from its assistant call).
#[test]
fn cut_snaps_back_to_the_span_user_message_over_budget() {
    let entries = vec![
        big("u1", Role::User, 3_000),
        big("a1", Role::Assistant, 3_000),
        big("t1", Role::ToolResult, 3_000),
        big("u2", Role::User, 4_000),
        big("a2", Role::Assistant, 5_000), // walk stops here (suffix 17k)
        big("t2", Role::ToolResult, 3_000),
        big("u3", Role::User, 3_000),
        big("a3", Role::Assistant, 3_000),
        big("t3", Role::ToolResult, 3_000),
    ];
    // Snapping to u2 keeps 21k — over KEEP_RECENT, but whole-spans only.
    assert_eq!(cut_point(&entries).as_deref(), Some("u2"));
}

/// The forward snap: when the walk stops before the second user message
/// (the head span's mass breaks the budget) the only backward candidate
/// is the untouchable head, so the cut moves FORWARD to that second user
/// — the dropped region still ends on complete spans.
#[test]
fn cut_snaps_forward_when_back_reaches_the_untouchable_head() {
    let entries = vec![
        big("u1", Role::User, 1_000),
        big("a1", Role::Assistant, 19_000), // walk breaks here: acc 22k
        big("t1", Role::ToolResult, 1_000),
        big("u2", Role::User, 1_000),
        big("a2", Role::Assistant, 1_000),
    ];
    // kept_from lands on t1; backward would only find the head u1, so
    // the cut snaps forward to u2 — dropping u1's whole span intact.
    assert_eq!(cut_point(&entries).as_deref(), Some("u2"));
}

/// History that fits the budget has nothing to summarize.
#[test]
fn short_history_declines() {
    let entries = vec![
        big("u1", Role::User, 1_000),
        big("a1", Role::Assistant, 1_000),
        big("t1", Role::ToolResult, 1_000),
    ];
    assert_eq!(cut_point(&entries), None);
}

/// A session whose only interior boundary would be the very first entry
/// declines: the head span is untouchable, so there is nothing to drop.
#[test]
fn single_span_session_declines() {
    let entries = vec![
        big("u1", Role::User, 8_000),
        big("a1", Role::Assistant, 8_000),
        big("t1", Role::ToolResult, 8_000),
    ];
    assert_eq!(cut_point(&entries), None);
}

/// The walk covers only the active region — from the previous
/// compaction's boundary onward. A big dead history behind a fresh
/// compaction does not re-trigger, and a big active region cuts inside
/// itself, never before the previous boundary.
#[test]
fn second_compaction_walks_only_the_active_region() {
    let declined = vec![
        big("u0", Role::User, 10_000),
        big("a0", Role::Assistant, 1_000),
        big("t0", Role::ToolResult, 1_000),
        big("u1", Role::User, 5_000),
        big("a1", Role::Assistant, 1_000),
        big("t1", Role::ToolResult, 1_000),
        compaction_entry("u1"),
        big("u2", Role::User, 1_000),
        big("a2", Role::Assistant, 1_000),
        big("t2", Role::ToolResult, 1_000),
    ];
    // Total history ~22k, but the active region since "u1" is 7k.
    assert_eq!(cut_point(&declined), None);

    let cut_inside = vec![
        big("u0", Role::User, 10_000),
        big("a0", Role::Assistant, 1_000),
        big("t0", Role::ToolResult, 1_000),
        big("u1", Role::User, 5_000),
        big("a1", Role::Assistant, 1_000),
        big("t1", Role::ToolResult, 1_000),
        compaction_entry("u1"),
        big("u2", Role::User, 7_000),
        big("a2", Role::Assistant, 7_000),
        big("t2", Role::ToolResult, 7_000),
        big("u3", Role::User, 7_000),
        big("a3", Role::Assistant, 1_000),
        big("t3", Role::ToolResult, 1_000),
    ];
    // Walk stops at t2 (suffix 16k); snapping back opens on u2.
    assert_eq!(cut_point(&cut_inside).as_deref(), Some("u2"));
}

#[test]
fn single_run_multi_turn_cuts_at_assistant_boundary_and_repeats() {
    let entries = vec![
        big("u1", Role::User, 1_000),
        big("a1", Role::Assistant, 6_000),
        big("t1", Role::ToolResult, 6_000),
        big("a2", Role::Assistant, 6_000),
        big("t2", Role::ToolResult, 6_000),
        big("a3", Role::Assistant, 6_000),
        big("t3", Role::ToolResult, 6_000),
    ];
    let first = cut_point(&entries).expect("assistant boundary cut");
    assert_eq!(first, "a2");

    // Second compaction after a1/t1 were summarized:
    let mut with_cmp = entries;
    with_cmp.push(compaction_entry("a2"));
    with_cmp.push(big("a4", Role::Assistant, 6_000));
    with_cmp.push(big("t4", Role::ToolResult, 6_000));
    let second = cut_point(&with_cmp).expect("second assistant boundary cut");
    assert_eq!(second, "a3");
}

#[test]
fn cut_point_with_counts_reasoning_when_replayed() {
    let mut a1 = big("a1", Role::Assistant, 1_000);
    if let Entry::Message { message, .. } = &mut a1 {
        message.reasoning = Some("r".repeat(12_000 * 4));
    }
    let entries = vec![
        big("u1", Role::User, 1_000),
        a1,
        big("t1", Role::ToolResult, 1_000),
        big("a2", Role::Assistant, 5_000),
        big("t2", Role::ToolResult, 5_000),
    ];
    // Without reasoning: 13k <= KEEP_RECENT -> None.
    assert_eq!(cut_point_with(&entries, ReplayReasoning::Never), None);
    // With reasoning: 25k > KEEP_RECENT -> cuts at a2.
    assert_eq!(
        cut_point_with(&entries, ReplayReasoning::Turn).as_deref(),
        Some("a2")
    );
}

#[test]
fn cut_point_with_turn_reasoning_ignores_prior_turns_and_abort_markers() {
    let mut a1 = big("a1", Role::Assistant, 1_000);
    if let Entry::Message { message, .. } = &mut a1 {
        message.reasoning = Some("r".repeat(12_000 * 4));
    }
    let entries = vec![
        big("u1", Role::User, 1_000),
        a1,
        big("t1", Role::ToolResult, 1_000),
        msg_entry("abort1", Role::User, "[aborted by user]", None),
        big("u2", Role::User, 1_000),
        big("a2", Role::Assistant, 5_000),
        big("t2", Role::ToolResult, 5_000),
    ];
    // In Turn mode, a1's reasoning is before u2 so it is not replayed (14k <= 20k -> None).
    assert_eq!(cut_point_with(&entries, ReplayReasoning::Turn), None);
    // In All mode, a1's reasoning IS replayed (26k > 20k) and must snap to u2, never abort1.
    assert_eq!(
        cut_point_with(&entries, ReplayReasoning::All).as_deref(),
        Some("u2")
    );
}
