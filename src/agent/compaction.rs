//! Context compaction (spec §14): trigger math, the cut-point walk, the
//! summarize call, and the loop hook that sequences them.
//!
//! Two constants, pi's defaults, no config: the reserve kept free in the
//! window and the recent history compaction preserves.

use anyhow::Context;
use tokio::sync::mpsc::Sender;

use super::session::{self, Entry, Message, Role, last_entry_id};
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
                + m.tool_calls
                    .iter()
                    .map(|c| c.function.name.chars().count() + c.function.arguments.chars().count())
                    .sum::<usize>()
        })
        .sum();
    (chars / 4) as u32
}

/// The trigger (§14): checked after a tool batch, before the next
/// request builds. `saturating_sub` keeps a window smaller than the
/// reserve from underflowing into a never-triggering comparison.
pub fn should_compact(context_tokens: u32, context_window: u32) -> bool {
    context_tokens > context_window.saturating_sub(RESERVE)
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
pub(super) fn entry_token_estimate(entry: &Entry) -> u32 {
    let chars = match entry {
        Entry::Message { message, .. } => message_token_chars(message),
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
/// snapped to a user-message boundary so a tool_result is never
/// separated from its assistant call. `None` when no boundary sits
/// inside the region: cutting there would orphan a span, so the session
/// waits for more history.
pub(super) fn cut_point(entries: &[Entry]) -> Option<String> {
    let active_start = active_start(entries);

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
        accumulated += entry_token_estimate(entry) as u64;
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

    // Snap to a user-message boundary (pi's span rule — a tool_result is
    // never separated from its assistant call). Backward first: the kept
    // region then includes the whole span the walk stopped in, erring
    // toward more history. But a final assistant answer carries no token
    // mass, so the walk often slides PAST a user boundary onto that
    // zero-size tail; when snapping back would reach the region's head
    // (drop nothing), snap FORWARD instead — the dropped region then
    // still ends on complete spans.
    let is_user =
        |e: &Entry| matches!(e, Entry::Message { message, .. } if message.role == Role::User);
    let backward = entries[active_start..=kept_from]
        .iter()
        .rposition(is_user)
        .map(|offset| active_start + offset)
        .filter(|&snapped| snapped > active_start);
    let forward = entries[kept_from..]
        .iter()
        .position(is_user)
        .map(|offset| kept_from + offset)
        .filter(|&snapped| snapped > active_start);
    let snapped = backward.or(forward)?;
    Some(last_entry_id(&entries[snapped]))
}

/// One extra, non-streaming LLM call over the wire view of the region
/// being dropped — which already collapses anything older into the
/// previous summary note, so nothing summarized before is lost. Errors
/// propagate; the caller degrades to a `Warned` and continues.
pub(super) async fn summarize(
    client: &LlmClient,
    model: &str,
    max_tokens: u32,
    dropped_wire: &[ChatMessage],
    instructions: Option<&str>,
) -> anyhow::Result<String> {
    let mut prompt = SUMMARIZE_INSTRUCTION.to_string();
    if let Some(focus) = instructions.filter(|s| !s.trim().is_empty()) {
        prompt.push_str(&format!("\nFocus: {focus}"));
    }
    let mut messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(prompt),
        ..ChatMessage::default()
    }];
    messages.extend_from_slice(dropped_wire);
    let response = client.send(model, &messages, max_tokens, &[]).await?;
    response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|text| !text.trim().is_empty())
        .context("summarize response had no content")
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
        let message = match self.compact_once(instructions).await? {
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
        let Some(first_kept_id) = cut_point(&self.entries) else {
            return Ok(CompactOutcome::NothingToDo);
        };
        let Some(kept_at) = self
            .entries
            .iter()
            .position(|e| last_entry_id(e) == first_kept_id)
        else {
            return Ok(CompactOutcome::NothingToDo);
        };
        // The dropped region's wire view comes from the CURRENT replay
        // view minus the kept suffix — not from the raw slice, whose
        // positional range cannot contain the previous `Compaction`
        // entry (append-only files put it after the entries it covers).
        // Slicing the full view this way carries the previous summary
        // note into this summarize call, so nothing summarized before is
        // lost. The kept suffix is always plain: any compaction inside
        // it positionally points its boundary further back, so
        // `to_wire` replays it raw — exactly the full view's suffix.
        let full_view = session::to_wire_with(&self.entries, self.replay_reasoning);
        let kept_view = session::to_wire_with(&self.entries[kept_at..], self.replay_reasoning);
        let dropped_wire = full_view[..full_view.len() - kept_view.len()].to_vec();
        match summarize(
            &self.client,
            &self.model,
            self.max_tokens,
            &dropped_wire,
            instructions,
        )
        .await
        {
            Ok(summary) => {
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
