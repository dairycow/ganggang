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

/// The trigger is a strict inequality against the line the reserve
/// leaves, and a window smaller than the reserve saturates to "always
/// past the line" rather than underflowing to "never".
#[test]
fn trigger_fires_past_the_reserved_line() {
    let window = 131_072u32;
    let line = window - 24_576;
    assert!(
        !should_compact(line, window, None, RESERVE),
        "exactly at the line holds"
    );
    assert!(
        should_compact(line + 1, window, None, RESERVE),
        "one past triggers"
    );
    assert!(!should_compact(0, window, None, RESERVE));
    // Degenerate window: the reserve scales down with it, and the trigger
    // still fires past the remaining line.
    assert!(should_compact(4, 4, None, RESERVE));

    // Threshold clamps below the ceiling when smaller, and is clamped
    // by it when larger.
    assert!(!should_compact(65_536, 262_144, Some(65_536), RESERVE));
    assert!(should_compact(65_537, 262_144, Some(65_536), RESERVE));
    assert!(!should_compact(line, window, Some(200_000), RESERVE));
    assert!(should_compact(line + 1, window, Some(200_000), RESERVE));
}

/// The reserve and kept budget scale down with the window, while a large
/// window keeps the fixed ceilings.
#[test]
fn reserve_and_keep_recent_scale_with_the_window() {
    assert_eq!(effective_reserve(131_072, RESERVE), 24_576);
    assert_eq!(compact_line(131_072, None, RESERVE), 106_496);
    assert_eq!(effective_keep_recent(131_072, None, RESERVE), 20_000);

    assert_eq!(effective_reserve(32_768, RESERVE), 12_288);
    assert_eq!(compact_line(32_768, None, RESERVE), 20_480);
    assert_eq!(effective_keep_recent(32_768, None, RESERVE), 6_826);

    // A 65k window: the line is 40,960, and a third of it leaves the
    // rest of the window free for the turns after a compaction.
    assert_eq!(compact_line(65_536, None, RESERVE), 40_960);
    assert_eq!(effective_keep_recent(65_536, None, RESERVE), 13_653);

    assert_eq!(effective_reserve(262_144, RESERVE), 24_576);
    assert_eq!(compact_line(262_144, Some(65_536), RESERVE), 65_536);
    assert_eq!(
        effective_keep_recent(262_144, Some(65_536), RESERVE),
        20_000
    );

    // A threshold below the ceiling still caps the kept budget at a third.
    assert_eq!(effective_keep_recent(131_072, Some(10_000), RESERVE), 3_333);
}

/// A request that asks for more completion tokens than [`RESERVE`] needs
/// that much more held free: the server holds its `max_tokens` out of the
/// window along with the input, and the line sits a headroom below the
/// input limit that leaves. A smaller `max_tokens` changes nothing, and
/// the cap keeps a small window workable whatever is asked for.
#[test]
fn a_larger_max_tokens_lowers_the_line() {
    assert_eq!(effective_reserve(262_144, 32_768), 32_768 + 8_192);
    assert_eq!(compact_line(262_144, None, 32_768), 221_184);
    assert_eq!(
        compact_line(262_144, None, 8_192),
        compact_line(262_144, None, RESERVE)
    );
    assert_eq!(effective_reserve(8_192, 65_536), 2_048 + 1_024);
}

/// An assistant entry with the given usage.
fn used(id: &str, prompt_tokens: u32, completion_tokens: u32) -> Entry {
    msg_entry(
        id,
        Role::Assistant,
        "done",
        Some(Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            ..Usage::default()
        }),
    )
}

/// Usage comes from the LAST assistant entry that has one — not the
/// first, and not from user or tool-result entries.
#[test]
fn context_tokens_come_from_the_last_assistant_usage() {
    let entries = vec![
        msg_entry("u1", Role::User, "hello", None),
        used("a1", 100, 20),
        msg_entry("t1", Role::ToolResult, &"o".repeat(400), None),
        used("a2", 300, 40),
    ];
    assert_eq!(context_tokens_of(&entries), Some(340));
    assert_eq!(context_tokens_of(&entries[..1]), None);
}

/// The entries appended since the measured response are counted too, at
/// chars/4: no usage has seen the tool results of the batch just run, and
/// they are what the next request adds.
#[test]
fn entries_appended_since_the_usage_count_toward_the_line() {
    // The numbers of a delegate run that died on a 400 from the server.
    // With a 16,384-token completion, a 262,144-token window takes at most
    // 245,760 tokens of input.
    let input_limit = 262_144 - RESERVE;
    let measured = 245_110 + 57;
    assert!(measured < input_limit, "the usage alone stayed under it");

    let entries = vec![
        msg_entry("u1", Role::User, "task", None),
        used("a1", 245_110, 57),
        big("t1", Role::ToolResult, 1_726),
    ];
    let tokens = context_tokens_of(&entries).expect("a1 carries usage");
    assert_eq!(tokens, measured + 1_726);
    assert!(tokens > input_limit, "the next request was past the limit");
    assert!(should_compact(tokens, 262_144, None, RESERVE));
}

/// Usage from before a compaction measured a context that no longer
/// exists, so it measures nothing: the count is `None` and the loop falls
/// back to estimating the request itself.
#[test]
fn usage_from_before_a_compaction_is_not_a_measure() {
    let entries = vec![
        msg_entry("u1", Role::User, "old", None),
        used("a1", 100, 20),
        compaction_entry("u2"),
        msg_entry("u2", Role::User, "new", None),
    ];
    assert_eq!(context_tokens_of(&entries), None);
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

    // An error entry is never sent, so however long it adds nothing: not
    // to the walk, and not to the count of entries appended since the last
    // usage.
    let failed = Entry::Error {
        id: "e9".into(),
        parent_id: None,
        timestamp: String::new(),
        error: "e".repeat(400),
    };
    assert_eq!(entry_token_estimate_with(&failed, true), 0);
    let measured = msg_entry(
        "a2",
        Role::Assistant,
        "answer",
        Some(Usage {
            prompt_tokens: 100,
            completion_tokens: 10,
            total_tokens: 110,
            ..Usage::default()
        }),
    );
    assert_eq!(context_tokens_of(&[measured, failed]), Some(110));
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

// ------------------------------------------------------- summarize call

/// A one-request server that records the body and answers with a summary,
/// so the summarize tests can see what the request actually carried.
struct Probe {
    base_url: String,
    body: std::sync::Arc<std::sync::Mutex<String>>,
    handle: std::thread::JoinHandle<()>,
}

fn read_one_request(stream: &mut std::net::TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match std::io::Read::read(stream, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let want = head
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if rest.len() >= want {
                        return rest.to_string();
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

impl Probe {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let body = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let recorded = std::sync::Arc::clone(&body);
        let handle = std::thread::spawn(move || {
            let stream = match listener.incoming().next() {
                Some(Ok(stream)) => stream,
                _ => return,
            };
            let mut stream = stream;
            let request = read_one_request(&mut stream);
            *recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = request;
            let answer = r#"{"choices":[{"message":{"role":"assistant","content":"a summary"}}]}"#;
            let _ = std::io::Write::write_all(
                &mut stream,
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
        });
        Self {
            base_url,
            body,
            handle,
        }
    }

    fn request_body(self) -> String {
        let _ = self.handle.join();
        self.body
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The summary is grounded in the working directory it was asked for, and
/// goes out on the cheap budget: capped output, downgraded reasoning.
#[tokio::test]
async fn summarize_grounds_the_prompt_in_cwd_and_uses_the_cheap_budget() {
    use std::sync::atomic::AtomicBool;

    let probe = Probe::spawn();
    let client = crate::llm::client::LlmClient::new(&probe.base_url)
        .unwrap()
        .with_reasoning_effort(Some("high".into()))
        .with_reasoning_budget_tokens(Some(4096));
    let dropped = vec![ChatMessage {
        role: "user".into(),
        content: Some("edit src/main.rs".into()),
        ..ChatMessage::default()
    }];
    let summary = super::summarize::summarize(
        &client,
        "test-model",
        16384,
        131_072,
        std::path::Path::new("/repo/root"),
        &dropped,
        None,
        &std::sync::Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect("summary")
    .expect("content");
    assert_eq!(summary, "a summary");

    let body = probe.request_body();
    assert!(body.contains(r"Working directory: /repo/root"), "{body}");
    assert!(
        body.contains("summarize the conversation so far for continuation"),
        "{body}"
    );
    assert!(body.contains(r#""max_tokens":4096"#), "{body}");
    assert!(body.contains(r#""reasoning_effort":"low""#), "{body}");
    assert!(body.contains(r#""reasoning_budget_tokens":512"#), "{body}");
}

// ------------------------------------------- bounded summary transcript

fn user(text: &str) -> ChatMessage {
    ChatMessage {
        role: "user".into(),
        content: Some(text.into()),
        ..ChatMessage::default()
    }
}

fn tool_result(id: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".into(),
        content: Some(text.into()),
        tool_call_id: Some(id.into()),
        ..ChatMessage::default()
    }
}

const OMITTED: &str = "\n[... 1000 chars omitted ...]\n";

/// A long tool result keeps its head and tail with a note between; a short
/// one is carried whole. Three-byte characters, so a cut on a byte that is
/// not a boundary would panic here.
#[test]
fn long_tool_results_are_capped_in_the_transcript() {
    let text: String = "漢字".repeat(2_500);
    let transcript = super::summarize::format_transcript_for_summary(&[
        user("look at this"),
        tool_result("call_1", &text),
        tool_result("call_2", "small"),
    ]);
    let section = transcript
        .split("Tool result (call_1):\n")
        .nth(1)
        .unwrap()
        .split("\n\nTool result (call_2)")
        .next()
        .unwrap();
    let head: String = text.chars().take(1_500).collect();
    let tail: String = text.chars().skip(2_500).collect();
    assert_eq!(section, format!("{head}{OMITTED}{tail}"));
    assert!(transcript.ends_with("Tool result (call_2):\nsmall"));
}

/// A transcript past the window's budget keeps its leading quarter and
/// its newest three-quarters, with a note naming what went missing, and
/// the result fits the budget.
#[test]
fn the_transcript_is_cut_to_the_window_budget() {
    // (10,240 - 6,144) * 3
    let budget = super::summarize::transcript_budget(10_240);
    assert_eq!(budget, 12_288);
    let old: String = "é".repeat(30_000);
    let wire = [user("original request"), user(&old), user("the newest")];
    let transcript = super::summarize::bounded_transcript(&wire, 10_240);

    let len = transcript.chars().count();
    assert!(len <= budget, "{len} > {budget}");
    let note_start = transcript.find("\n\n[... ").expect("the note");
    let note_end = transcript.find(" ...]\n\n").unwrap() + " ...]\n\n".len();
    let note = &transcript[note_start..note_end];
    let (lead, newest) = transcript.split_once(note).unwrap();
    assert!(lead.starts_with("User:\noriginal request"), "{lead}");
    assert!(newest.ends_with("User:\nthe newest"), "{newest}");
    // The kept body splits a quarter to the lead and three quarters to
    // the newest; only the note sits outside it.
    let kept = lead.chars().count() + newest.chars().count();
    assert_eq!(
        lead.chars().count(),
        kept / 4,
        "a leading quarter of the kept body"
    );
    assert_eq!(
        kept + note.chars().count(),
        budget,
        "the cut fills the budget"
    );
    let omitted = 30_000
        + 3 * "User:\n".chars().count()
        + 2 * "\n\n".chars().count()
        + "original request".chars().count()
        + "the newest".chars().count()
        - len
        + note.chars().count();
    assert_eq!(
        note,
        format!(
            "\n\n[... {omitted} chars of older transcript omitted to fit summary window ...]\n\n"
        )
    );
}

/// A transcript inside the budget is carried whole, and a window with no
/// room to spare still carries a short one.
#[test]
fn short_transcripts_and_small_windows() {
    let wire = [user("short")];
    assert_eq!(
        super::summarize::bounded_transcript(&wire, 10_240),
        "User:\nshort"
    );
    assert_eq!(super::summarize::transcript_budget(0), 4_096);
    assert_eq!(super::summarize::transcript_budget(6_144), 4_096);
    assert_eq!(super::summarize::transcript_budget(7_144), 4_096);
    assert_eq!(super::summarize::transcript_budget(8_144), 6_000);
    let long: String = "x".repeat(5_000);
    let cut = super::summarize::bounded_transcript(&[user(&long)], 0);
    assert!(cut.chars().count() <= 4_096);
    assert!(cut.contains("chars of older transcript omitted to fit summary window"));
}

/// The request itself carries the bounded transcript: the window is
/// passed through to the summariser.
#[tokio::test]
async fn summarize_sends_a_transcript_the_window_fits() {
    use std::sync::atomic::AtomicBool;

    let probe = Probe::spawn();
    let client = crate::llm::client::LlmClient::new(&probe.base_url).unwrap();
    let dropped = [
        user("ORIGINAL-REQUEST"),
        tool_result("call_1", &"x".repeat(20_000)),
        user("NEWEST-MESSAGE"),
    ];
    let summary = super::summarize::summarize(
        &client,
        "test-model",
        16384,
        10_240,
        std::path::Path::new("/repo/root"),
        &dropped,
        None,
        &std::sync::Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect("summary")
    .expect("content");
    assert_eq!(summary, "a summary");

    let body = probe.request_body();
    let prompt =
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
    let transcript = prompt
        .split("<conversation>\n")
        .nth(1)
        .unwrap()
        .split("\n</conversation>")
        .next()
        .unwrap();
    assert!(
        transcript.starts_with("User:\nORIGINAL-REQUEST"),
        "{transcript}"
    );
    assert!(
        transcript.ends_with("User:\nNEWEST-MESSAGE"),
        "{transcript}"
    );
    assert!(
        transcript.chars().count() <= 12_288,
        "{} chars went out",
        transcript.chars().count()
    );
}
