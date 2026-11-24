//! Context compaction (spec §14): trigger math, the cut-point walk, the
//! summarize call, and the loop hook that sequences them.
//!
//! Two pi defaults (`RESERVE` and `KEEP_RECENT`), plus an optional lower
//! `compact_threshold_tokens` ceiling for local-compute KV residency.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use tokio::sync::mpsc::Sender;

use super::session::{self, Entry, Message, ReplayReasoning, Role, last_entry_id};
use crate::events::AgentEvent;
use crate::llm::client::LlmClient;
use crate::llm::types::ChatMessage;

/// Tokens held free in the context window: compaction triggers once the
/// conversation crowds past this line, so the next request still fits.
pub const RESERVE: u32 = 16_384;

/// How much recent history compaction tries to keep, in tokens.
pub const KEEP_RECENT: u32 = 20_000;

/// Context tokens per the spec: from the last response's `usage` — the
/// request the run just made is the best measure of the one it is about
/// to make. `None` when no assistant entry carries usage yet.
pub fn context_tokens_of(entries: &[Entry]) -> Option<u32> {
    entries.iter().rev().find_map(|entry| match entry {
        Entry::Message {
            message:
                Message {
                    role: Role::Assistant,
                    usage: Some(u),
                    ..
                },
            ..
        } => Some(u.prompt_tokens + u.completion_tokens),
        _ => None,
    })
}

/// The chars/4 fallback, for servers that report no usage: the wire
/// view of the request is what would be counted, so that is what is
/// estimated — content plus tool-call names and argument text.
pub fn estimate_wire_tokens(messages: &[ChatMessage]) -> u32 {
    let chars: usize = messages
        .iter()
        .map(|m| {
            m.content.as_deref().map_or(0, |c| c.chars().count())
                + m.reasoning_content
                    .as_deref()
                    .map_or(0, |r| r.chars().count())
                + m.tool_calls
                    .iter()
                    .map(|c| c.function.name.chars().count() + c.function.arguments.chars().count())
                    .sum::<usize>()
        })
        .sum();
    (chars / 4) as u32
}

/// Effective compaction line: `min(threshold, window - RESERVE)` when a
/// positive threshold is configured, otherwise `window - RESERVE`.
pub fn compact_line(context_window: u32, threshold: Option<u32>) -> u32 {
    let ceiling = context_window.saturating_sub(RESERVE);
    match threshold.filter(|&t| t > 0) {
        Some(t) => t.min(ceiling),
        None => ceiling,
    }
}

/// The trigger (§14): checked after a tool batch, before the next
/// request builds. `saturating_sub` keeps a window smaller than the
/// reserve from underflowing into a never-triggering comparison.
pub fn should_compact(context_tokens: u32, context_window: u32, threshold: Option<u32>) -> bool {
    context_tokens > compact_line(context_window, threshold)
}

/// Per-entry token estimate for the cut-point walk (§14: "walk back
/// through entries accumulating tokens"): the entry's text mass —
/// content, tool-call names and arguments, summary text — at chars/4.
/// Usage numbers are deliberately NOT used here: they measure whole
/// requests and would double-count every earlier entry per turn.
///
/// Known drift: chars/4 underestimates token-heavy scripts (CJK runs
/// ~1 token/char under Qwen tokenizers), so a kept region can hold up
/// to ~4× `KEEP_RECENT` real tokens. The direction is safe for large
/// windows; under a small window, keep the estimate honest by matching
/// `context_window` to the server's real one.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn entry_token_estimate(entry: &Entry) -> u32 {
    entry_token_estimate_with(entry, false)
}

pub(super) fn entry_token_estimate_with(entry: &Entry, include_reasoning: bool) -> u32 {
    let chars = match entry {
        Entry::Message { message, .. } => {
            message_token_chars(message)
                + if include_reasoning {
                    message
                        .reasoning
                        .as_deref()
                        .map_or(0, |r| r.chars().count())
                } else {
                    0
                }
        }
        Entry::Compaction { summary, .. } => summary.chars().count(),
    };
    (chars / 4) as u32
}

fn message_token_chars(m: &Message) -> usize {
    m.content.as_deref().map_or(0, |c| c.chars().count())
        + m.tool_calls
            .iter()
            .map(|c| c.name.chars().count() + c.args.to_string().chars().count())
            .sum::<usize>()
}

/// The instruction §14 words for the summarize call.
const SUMMARIZE_INSTRUCTION: &str = "summarize the conversation so far for continuation; preserve goals, decisions, file paths, open tasks";

/// Index where the ACTIVE region starts: the entry the previous
/// compaction kept from, or 0 when none has happened. Everything
/// before it is already summarized dead history.
fn active_start(entries: &[Entry]) -> usize {
    entries
        .iter()
        .rposition(|e| matches!(e, Entry::Compaction { .. }))
        .and_then(|idx| match &entries[idx] {
            Entry::Compaction { first_kept_id, .. } => entries
                .iter()
                .position(|e| &last_entry_id(e) == first_kept_id),
            _ => None,
        })
        .unwrap_or(0)
}

/// The cut point (§14): the id of the first entry the compacted view
/// keeps. The walk covers only the ACTIVE region — from the previous
/// compaction's boundary onward, so a second compaction re-summarizes
/// what accumulated since, never the already-summarized dead history —
/// and keeps the maximal suffix estimated at or under `KEEP_RECENT`,
/// snapped to a user-message boundary (or an assistant-message boundary in
/// single-prompt multi-turn runs) so a tool_result is never separated from
/// its assistant call. `None` when no boundary sits inside the region:
/// cutting there would orphan a span, so the session waits for more history.
#[allow(dead_code)]
pub(super) fn cut_point(entries: &[Entry]) -> Option<String> {
    cut_point_with(entries, ReplayReasoning::Never)
}

pub(super) fn cut_point_with(
    entries: &[Entry],
    replay_reasoning: ReplayReasoning,
) -> Option<String> {
    let active_start = active_start(entries);
    let is_user = |e: &Entry| {
        matches!(
            e,
            Entry::Message { message, .. }
                if message.role == Role::User
                    && message.content.as_deref() != Some("[aborted by user]")
        )
    };
    let last_user_idx = entries[active_start..]
        .iter()
        .rposition(is_user)
        .map(|offset| active_start + offset);

    // Walk back accumulating estimates; `kept_from` is the oldest index
    // whose suffix still fits the budget. It starts at the newest entry:
    // even a single oversized entry is kept — the cut never empties the
    // recent region.
    let mut accumulated: u64 = 0;
    let mut kept_from = entries.len().saturating_sub(1);
    for (index, entry) in entries
        .iter()
        .enumerate()
        .rev()
        .take(entries.len().saturating_sub(active_start))
    {
        let include_reasoning = match replay_reasoning {
            ReplayReasoning::Never => false,
            ReplayReasoning::All => true,
            ReplayReasoning::Turn => last_user_idx.is_none_or(|u| index > u),
        };
        accumulated += entry_token_estimate_with(entry, include_reasoning) as u64;
        if accumulated > KEEP_RECENT as u64 {
            break;
        }
        kept_from = index;
    }
    if kept_from <= active_start {
        // The active region fits the budget (or only the untouchable
        // head would have to go): nothing new to summarize.
        return None;
    }

    // Tier 1: snap to a user-message boundary (backward first, then forward).
    let backward_user = entries[active_start..=kept_from]
        .iter()
        .rposition(is_user)
        .map(|offset| active_start + offset)
        .filter(|&snapped| snapped > active_start);
    let forward_user = entries[kept_from..]
        .iter()
        .position(is_user)
        .map(|offset| kept_from + offset)
        .filter(|&snapped| snapped > active_start);
    if let Some(snapped) = backward_user.or(forward_user) {
        return Some(last_entry_id(&entries[snapped]));
    }

    // Tier 2 (single-prompt multi-turn fallback): snap to an assistant
    // entry strictly after `active_start` that has at least one earlier
    // assistant entry in `entries[active_start..idx]`.
    let is_assistant =
        |e: &Entry| matches!(e, Entry::Message { message, .. } if message.role == Role::Assistant);
    let has_prior_assistant =
        |idx: usize| idx > active_start && entries[active_start..idx].iter().any(is_assistant);
    let backward_asst = entries[active_start..=kept_from]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, e)| {
            let idx = active_start + offset;
            (is_assistant(e) && has_prior_assistant(idx)).then_some(idx)
        });
    let forward_asst = entries[kept_from..]
        .iter()
        .enumerate()
        .find_map(|(offset, e)| {
            let idx = kept_from + offset;
            (is_assistant(e) && has_prior_assistant(idx)).then_some(idx)
        });
    let snapped = backward_asst.or(forward_asst)?;
    Some(last_entry_id(&entries[snapped]))
}

fn format_transcript_for_summary(dropped_wire: &[ChatMessage]) -> String {
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
/// the region being dropped — which already collapses anything older into the
/// previous summary note, so nothing summarized before is lost. Returns
/// `Ok(None)` if aborted mid-request.
pub(super) async fn summarize(
    client: &LlmClient,
    model: &str,
    max_tokens: u32,
    dropped_wire: &[ChatMessage],
    instructions: Option<&str>,
    abort: &Arc<AtomicBool>,
) -> anyhow::Result<Option<String>> {
    let mut prompt = SUMMARIZE_INSTRUCTION.to_string();
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
        _ = super::loop_::wait_for_abort(abort) => return Ok(None),
        res = client.send(model, &messages, max_tokens, &[]) => res?,
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

/// What one compaction attempt did — the callers turn this into events
/// (the loop) or interface feedback (`/compact`), each in their own
/// voice. Failure is a variant, never an `Err`: §14 kills no runs.
#[derive(Debug)]
pub enum CompactOutcome {
    /// No boundary inside the active region: nothing to summarize yet.
    NothingToDo,
    Compacted {
        dropped: usize,
        first_kept_id: String,
    },
    Failed(String),
    Aborted,
}

impl super::Agent {
    /// The loop's §14 hook: run one compaction when the trigger line is
    /// past, reporting through the event stream (§17: truncation is
    /// visible even when it is routine). A session past the line with no
    /// cuttable span (single-run sessions, or an active region still
    /// inside `KEEP_RECENT`) says so once per run — silently requesting
    /// past the configured window would otherwise end in an unexplained
    /// server error.
    pub(super) async fn maybe_compact(
        &mut self,
        instructions: Option<&str>,
        events: &mut Sender<AgentEvent>,
        noop_warned: &mut bool,
    ) -> anyhow::Result<()> {
        let outcome = self.compact_once(instructions).await?;
        // A retry the summary request needed is news whatever the attempt
        // concluded, and it is this run's: said here, with the rest.
        self.relay_notices(events).await?;
        let message = match outcome {
            CompactOutcome::NothingToDo => {
                if !*noop_warned {
                    *noop_warned = true;
                    events
                        .send(AgentEvent::Warned {
                            message: "context is past the compaction line but no cut \
                                      point exists yet — the next requests still grow \
                                      toward the window edge"
                                .into(),
                        })
                        .await?;
                }
                return Ok(());
            }
            CompactOutcome::Compacted {
                dropped,
                first_kept_id,
            } => format!(
                "context compacted: {dropped} entries summarized (kept from {first_kept_id})"
            ),
            CompactOutcome::Failed(detail) => {
                format!("compaction failed ({detail}); continuing un-compacted")
            }
            CompactOutcome::Aborted => return Ok(()),
        };
        events.send(AgentEvent::Warned { message }).await?;
        Ok(())
    }

    /// One compaction attempt, interface-agnostic: cut, summarize,
    /// record the `Compaction` entry (persist before memory, §13).
    /// Session-I/O errors still abort the caller — that is the §17
    /// run-killing class — but summary failures come back as
    /// [`CompactOutcome::Failed`].
    pub async fn compact_once(
        &mut self,
        instructions: Option<&str>,
    ) -> anyhow::Result<CompactOutcome> {
        let Some(first_kept_id) = cut_point_with(&self.entries, self.replay_reasoning) else {
            return Ok(CompactOutcome::NothingToDo);
        };
        let Some(kept_at) = self
            .entries
            .iter()
            .position(|e| last_entry_id(e) == first_kept_id)
        else {
            return Ok(CompactOutcome::NothingToDo);
        };
        let summarize_replay = match self.replay_reasoning {
            ReplayReasoning::All => ReplayReasoning::All,
            _ => ReplayReasoning::Never,
        };
        let full_view = session::to_wire_with(&self.entries, summarize_replay);
        let kept_view = session::to_wire_with(&self.entries[kept_at..], summarize_replay);
        let dropped_wire = full_view[..full_view.len().saturating_sub(kept_view.len())].to_vec();
        if self.abort.load(Ordering::Acquire) {
            return Ok(CompactOutcome::Aborted);
        }
        match summarize(
            &self.client,
            &self.model,
            self.max_tokens,
            &dropped_wire,
            instructions,
            &self.abort,
        )
        .await
        {
            Ok(None) => Ok(CompactOutcome::Aborted),
            Ok(Some(summary)) => {
                // Only the newly-summarized live entries count: `kept_at`
                // includes the dead history earlier compactions already
                // folded into the summary this call carried forward.
                let dropped = kept_at - active_start(&self.entries);
                let entry = Entry::Compaction {
                    id: session::new_entry_id(),
                    parent_id: self.entries.last().map(last_entry_id),
                    timestamp: session::now_iso(),
                    summary,
                    first_kept_id: first_kept_id.clone(),
                };
                if let Some(store) = &mut self.store {
                    store.append(&entry)?;
                }
                self.entries.push(entry);
                Ok(CompactOutcome::Compacted {
                    dropped,
                    first_kept_id,
                })
            }
            Err(e) => Ok(CompactOutcome::Failed(format!("{e:#}"))),
        }
    }
}

#[cfg(test)]
mod tests;
