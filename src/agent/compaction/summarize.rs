//! The summarize call §14 adds: the text transcript of the region being
//! dropped, the instruction that grounds it in the session's working
//! directory, and the one cheap LLM call that turns the region into a
//! continuation summary.
//!
//! The transcript is bounded twice over: one tool result carries at most
//! its head and tail, and the whole transcript is cut to what the context
//! window holds beside the summary itself. The leading quarter survives the
//! cut, which is where the previous continuation summary and the original
//! request sit.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context;

use crate::llm::client::{LlmClient, SUMMARY_MAX_TOKENS};
use crate::llm::types::ChatMessage;

/// The instruction §14 words for the summarize call. It names what a
/// continuation needs, and the one thing a summary cannot stand in for:
/// the source lines the compacted region no longer carries.
pub(super) const SUMMARIZE_INSTRUCTION: &str = "summarize the conversation so far for continuation; preserve goals, decisions, exact file paths with key line ranges and symbols already located (already edited vs remaining), open tasks, and latest errors; note that compacted source lines are no longer in context so targeted read(offset, limit) is required before editing";

/// The characters of one tool result the transcript keeps: its first
/// [`TOOL_RESULT_HEAD_CHARS`] and its last [`TOOL_RESULT_TAIL_CHARS`] once
/// it is longer than this.
const TOOL_RESULT_MAX_CHARS: usize = 4_000;
const TOOL_RESULT_HEAD_CHARS: usize = 1_500;
const TOOL_RESULT_TAIL_CHARS: usize = 2_500;

/// Tokens the summary window holds for its instruction, the working
/// directory line and a little reasoning, besides the capped output.
const SUMMARY_PROMPT_TOKENS: u32 = 2048;

pub(super) fn format_transcript_for_summary(dropped_wire: &[ChatMessage]) -> String {
    let mut sections = Vec::with_capacity(dropped_wire.len());
    for msg in dropped_wire {
        match msg.role.as_str() {
            "user" => {
                let text = msg.content.as_deref().unwrap_or("").trim();
                if !text.is_empty() {
                    sections.push(format!("User:\n{text}"));
                }
            }
            "assistant" => {
                let mut part = String::from("Assistant:");
                if let Some(reason) = msg.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
                    part.push_str(&format!("\n[thinking]\n{reason}"));
                }
                if let Some(text) = msg.content.as_deref().filter(|c| !c.is_empty()) {
                    part.push_str(&format!("\n{text}"));
                }
                for tc in &msg.tool_calls {
                    part.push_str(&format!(
                        "\n[tool_call {}({})]",
                        tc.function.name, tc.function.arguments
                    ));
                }
                sections.push(part);
            }
            "tool" => {
                let id = msg.tool_call_id.as_deref().unwrap_or("unknown");
                let text = cap_tool_result(msg.content.as_deref().unwrap_or(""));
                sections.push(format!("Tool result ({id}):\n{text}"));
            }
            other => {
                let text = msg.content.as_deref().unwrap_or("");
                sections.push(format!("{other}:\n{text}"));
            }
        }
    }
    sections.join("\n\n")
}

/// The most of the dropped region the summary prompt carries, in
/// characters. The window has to hold the summary's output and the rest of
/// the prompt besides the transcript, and three characters per token is
/// the conservative side of the estimate. A window too small to leave room
/// still carries a short transcript.
pub(super) fn transcript_budget(context_window: u32) -> usize {
    (context_window.saturating_sub(SUMMARY_MAX_TOKENS + SUMMARY_PROMPT_TOKENS) as usize)
        .saturating_mul(3)
        .max(4_096)
}

/// The dropped region as the summary prompt carries it: formatted, every
/// tool result capped, and the whole held to what `context_window` fits.
pub(super) fn bounded_transcript(dropped_wire: &[ChatMessage], context_window: u32) -> String {
    let transcript = format_transcript_for_summary(dropped_wire);
    cap_transcript(&transcript, transcript_budget(context_window))
}

/// A tool result reduced to its head and tail once it is longer than
/// [`TOOL_RESULT_MAX_CHARS`]: the start says what was asked, the end how
/// it went.
fn cap_tool_result(text: &str) -> Cow<'_, str> {
    if text.chars().count() <= TOOL_RESULT_MAX_CHARS {
        return Cow::Borrowed(text);
    }
    Cow::Owned(keep_edges(
        text,
        TOOL_RESULT_HEAD_CHARS,
        TOOL_RESULT_TAIL_CHARS,
        |omitted| format!("\n[... {omitted} chars omitted ...]\n"),
    ))
}

/// `transcript` with its older middle left out once it is longer than
/// `max_chars`: the leading quarter stays, along with the newest
/// three-quarters, and a note in the gap says how much went missing.
fn cap_transcript(transcript: &str, max_chars: usize) -> String {
    let len = transcript.chars().count();
    if len <= max_chars {
        return transcript.to_string();
    }
    let note = |omitted: usize| {
        format!(
            "\n\n[... {omitted} chars of older transcript omitted to fit summary window ...]\n\n"
        )
    };
    // The note's own length, at its longest: the omitted count has at
    // most as many digits as the transcript has characters.
    let note_room = note(len).chars().count();
    let room = max_chars.saturating_sub(note_room);
    keep_edges(transcript, room / 4, room - room / 4, note)
}

/// The first `head` and the last `tail` characters of `text` with
/// `marker(n)` between them, `n` being the characters left out. Cuts on
/// character boundaries only.
fn keep_edges(
    text: &str,
    head: usize,
    tail: usize,
    marker: impl FnOnce(usize) -> String,
) -> String {
    let len = text.chars().count();
    let tail_start = len.saturating_sub(tail);
    if head >= tail_start {
        return text.to_string();
    }
    let head_end = text
        .char_indices()
        .nth(head)
        .map_or(text.len(), |(at, _)| at);
    let tail_at = text
        .char_indices()
        .nth(tail_start)
        .map_or(text.len(), |(at, _)| at);
    let mut kept = String::with_capacity(head_end + text.len() - tail_at + 64);
    kept.push_str(&text[..head_end]);
    kept.push_str(&marker(len - head - tail));
    kept.push_str(&text[tail_at..]);
    kept
}

/// One extra, non-streaming LLM call over a bounded text transcript of
/// the region being dropped, which already collapses anything older into
/// the previous summary note, so nothing summarized before is lost.
/// Returns `Ok(None)` if aborted mid-request.
///
/// The working directory heads the prompt: a summary that names paths has
/// to name real ones, and the dropped transcript is the only other thing
/// the call sees. The request goes through [`LlmClient::send_summary`], so
/// it carries no tools, a capped output budget, and a downgraded reasoning
/// effort (the cheap call compaction should be). `context_window` bounds
/// the transcript to what that window holds beside the summary.
#[allow(clippy::too_many_arguments)]
pub(super) async fn summarize(
    client: &LlmClient,
    model: &str,
    max_tokens: u32,
    context_window: u32,
    cwd: &Path,
    dropped_wire: &[ChatMessage],
    instructions: Option<&str>,
    abort: &Arc<AtomicBool>,
) -> anyhow::Result<Option<String>> {
    let mut prompt = format!("Working directory: {}\n", cwd.display());
    prompt.push_str(SUMMARIZE_INSTRUCTION);
    if let Some(focus) = instructions.filter(|s| !s.trim().is_empty()) {
        prompt.push_str(&format!("\nFocus: {focus}"));
    }
    let transcript = bounded_transcript(dropped_wire, context_window);
    prompt.push_str(&format!(
        "\n\n<conversation>\n{transcript}\n</conversation>\n\nProvide the continuation summary now."
    ));
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt),
        ..ChatMessage::default()
    }];
    let response = tokio::select! {
        biased;
        _ = crate::agent::loop_::wait_for_abort(abort) => return Ok(None),
        res = client.send_summary(model, &messages, max_tokens) => res?,
    };
    response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|text| !text.trim().is_empty())
        .context("summarize response had no content")
        .map(Some)
}
