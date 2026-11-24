//! The summarize call §14 adds: the text transcript of the region being
//! dropped, the instruction that grounds it in the session's working
//! directory, and the one cheap LLM call that turns the region into a
//! continuation summary.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Context;

use crate::llm::client::LlmClient;
use crate::llm::types::ChatMessage;

/// The instruction §14 words for the summarize call. It names what a
/// continuation needs, and the one thing a summary cannot stand in for:
/// the source lines the compacted region no longer carries.
pub(super) const SUMMARIZE_INSTRUCTION: &str = "summarize the conversation so far for continuation; preserve goals, decisions, exact file paths (already edited vs remaining), open tasks; note that compacted source lines are no longer in context so targeted reads are needed before editing";

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
                let text = msg.content.as_deref().unwrap_or("");
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

/// One extra, non-streaming LLM call over a text-serialized transcript of
/// the region being dropped — which already collapses anything older into
/// the previous summary note, so nothing summarized before is lost.
/// Returns `Ok(None)` if aborted mid-request.
///
/// The working directory heads the prompt: a summary that names paths has
/// to name real ones, and the dropped transcript is the only other thing
/// the call sees. The request goes through [`LlmClient::send_summary`], so
/// it carries no tools, a capped output budget, and a downgraded reasoning
/// effort — the cheap call compaction should be.
pub(super) async fn summarize(
    client: &LlmClient,
    model: &str,
    max_tokens: u32,
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
    let transcript = format_transcript_for_summary(dropped_wire);
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
