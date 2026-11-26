//! The turn engine: request → tool calls → results → repeat (spec §9).
//!
//! Straight-line async on purpose. Tool failures are values the model can
//! read; only transport and session errors propagate. Runaway loops are
//! stopped by the user via the abort flag (Esc / Ctrl+C / SIGTERM) or an
//! optional `--max-turns` / `GG_MAX_TURNS` cap. The cap bounds one thing
//! more than it did: a truncated answer earns a few "keep going" turns
//! ([`cut`]), and those count toward it like any other.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

use super::Agent;
use super::compaction;
use super::session::{self, Role};
use super::tools::Cancel;

mod ask;
mod cut;
mod dispatch;
mod record;

pub(crate) async fn wait_for_abort(abort: &Arc<AtomicBool>) {
    loop {
        if abort.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(super) fn soft_nudge_turn(limit: u32) -> u32 {
    ((limit as u64 * 4) / 5).clamp(2, limit as u64) as u32
}

/// The wrap-up notice for a turn, earned at most twice per run: on the 80%
/// line and on the final turn. `None` for every turn between them, so the
/// request grows by appending instead of rewriting a nudged message.
pub(super) fn wrap_up_notice(turns: u32, limit: u32) -> Option<String> {
    if limit < 2 || (turns != limit && turns != soft_nudge_turn(limit)) {
        return None;
    }
    let remaining = limit.saturating_sub(turns);
    Some(if remaining == 0 {
        format!(
            "[system notice: this is your final turn ({turns}/{limit}, 0 turn(s) remaining). Do not call any tools; provide your final response now.]"
        )
    } else {
        format!(
            "[system notice: you are on turn {turns} of {limit} ({remaining} turn(s) remaining). Wrap up your work and return your final response soon without starting new exploration.]"
        )
    })
}

impl Agent {
    /// Record an abort marker in the session and emit `Warned` + `RunAborted`.
    pub(super) async fn emit_abort(
        &mut self,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        self.push_message(Role::User, Some("[aborted by user]".into()))?;
        events
            .send(AgentEvent::Warned {
                message: "run aborted".into(),
            })
            .await?;
        events.send(AgentEvent::RunAborted).await?;
        Ok(())
    }

    /// Run one user input to completion: turn 1 asks the model, tool calls
    /// are dispatched (concurrently for contiguous read-only calls), each
    /// batch loops back into a new turn until the model answers without
    /// wanting tools.
    ///
    /// A turn that falls short — the stream was cut, the answer hit
    /// `max_tokens`, or the user stopped it — is decided before anything
    /// else happens to it ([`cut`]): its tool calls never run, and the run
    /// either asks for the rest or ends with the partial answer standing.
    pub async fn run(
        &mut self,
        input: String,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        self.run_inner(input, events, true).await
    }

    /// Entry point for a nested run that shares the parent run's stop flag
    /// (the `task` tool hands the parent's `Arc<AtomicBool>` to the child
    /// agent): the flag is owned and cleared by the top-level run, so this
    /// entry point never clears it. A stop latched between the parent's
    /// last abort check and the child's start is therefore honored at the
    /// child's first check instead of being erased, and the child ends with
    /// the abort shape without asking the model at all.
    pub async fn run_nested(
        &mut self,
        input: String,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        self.run_inner(input, events, false).await
    }

    /// The shared body of [`Self::run`] and [`Self::run_nested`]: the same turn loop,
    /// abort checks, and `emit_abort` behaviour. `reset_abort` clears the
    /// stop flag on entry so a latch left by an earlier run cannot
    /// phantom-abort this one; a nested run passes `false` because its
    /// flag belongs to the parent.
    async fn run_inner(
        &mut self,
        input: String,
        events: &mut Sender<AgentEvent>,
        reset_abort: bool,
    ) -> anyhow::Result<()> {
        // Every top-level run starts with a clean stop-request state: a
        // flag latched by an earlier run (or an interface path that forgot
        // to reset it) would phantom-abort this one between tool calls.
        // A nested run (`reset_abort: false`) shares its parent's flag and
        // leaves it alone.
        if reset_abort {
            self.abort.store(false, Ordering::Release);
        }
        // The tool-facing view of that same flag: one handle for every call
        // this run dispatches, so a running tool notices the stop request
        // instead of only being skipped between calls.
        let cancel = Cancel::new(self.abort.clone());
        // Set when a batch reports trigger-true/NothingToDo, so the
        // "past the line, no cut point" warning fires once, not per batch.
        let mut noop_compaction_warned = false;
        self.push_message(Role::User, Some(input))?;
        // One session id for every hook payload this run emits; `None` is
        // `--no-session` (§22).
        let session_id = self.store.as_ref().map(|s| s.id().to_string());
        events.send(AgentEvent::RunStarted).await?;
        for warning in std::mem::take(&mut self.skill_warnings)
            .into_iter()
            .chain(std::mem::take(&mut self.agents_warnings))
        {
            events.send(AgentEvent::Warned { message: warning }).await?;
        }

        let mut turns = 0u32;
        // Wrap-up notices keyed by the entry id they ride: recorded once and
        // replayed from then on, so the request stays append-only past the
        // 80% line and the provider's KV prefix cache survives the nudges.
        // Nothing here is persisted — the session file never sees a notice.
        let mut notices: HashMap<String, String> = HashMap::new();
        // Continuations this run has spent on a short answer (§ cut).
        let mut continuations = 0u32;
        loop {
            if self.abort.load(Ordering::Acquire) {
                self.emit_abort(events).await?;
                return Ok(());
            }
            turns += 1;
            events.send(AgentEvent::TurnStarted).await?;

            let system = self.system_prompt();
            if turns == 1 && std::env::var("GG_DEBUG_PROMPT").is_ok_and(|v| v == "1") {
                eprintln!("{system}");
            }
            let mut messages = vec![ChatMessage {
                role: "system".to_string(),
                content: Some(system),
                ..ChatMessage::default()
            }];
            if let Some(limit) = self.max_turns
                && limit >= 2
                && turns == soft_nudge_turn(limit)
            {
                events
                    .send(AgentEvent::Warned {
                        message: format!(
                            "approaching max_turns ({turns}/{limit}); nudging model to wrap up"
                        ),
                    })
                    .await?;
            }
            let mut trailing_notice: Option<String> = None;
            if let Some(limit) = self.max_turns
                && let Some(notice) = wrap_up_notice(turns, limit)
            {
                // The notice rides the entry backing the last wire message
                // (the last tool result, or the continuation instruction),
                // where it stays for the rest of the run. A message of its
                // own after either would put two of a kind in a row, which
                // strict providers refuse.
                match session::last_noticeable_entry_id(&self.entries) {
                    Some(id) => {
                        notices
                            .entry(id)
                            .and_modify(|held| *held = format!("{held}\n\n{notice}"))
                            .or_insert(notice);
                    }
                    // Last wire message is an assistant turn: a trailing
                    // user message is the only shape left that fits, and it
                    // has to go after the history, not before it.
                    None => trailing_notice = Some(notice),
                }
            }
            messages.extend(session::to_wire_with_notices(
                &self.entries,
                self.replay_reasoning,
                &notices,
            ));
            if let Some(notice) = trailing_notice {
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Some(notice),
                    ..ChatMessage::default()
                });
            }

            // The request, with the stop request wired in: a stream stops
            // when the user asks and comes back with what arrived, and a
            // non-streaming wait is raced against the same flag.
            let outcome = self.ask(&messages, events).await?;
            // Read before `from_response` consumes the response: the wire
            // type stays the single place finish reasons and timings exist.
            let timings = outcome.response.timings.clone();
            let truncated = outcome
                .response
                .choices
                .first()
                .and_then(|c| c.finish_reason.as_deref())
                == Some("length");
            // A stop request outranks every other reason a turn fell short:
            // the user has already disowned the next request, so the
            // partial stays and nothing is asked for. Otherwise a `length`
            // finish explains itself even when the stream ended cleanly, and
            // either way the answer is a prefix and the turn is short.
            let short = if outcome.aborted {
                Some(cut::Cut::Aborted)
            } else if truncated {
                Some(cut::Cut::MaxTokens)
            } else if !outcome.complete {
                Some(cut::Cut::StreamEnded)
            } else {
                None
            };
            let (message, warnings) = session::from_response(outcome.response)?;
            let turn_usage = message.usage;
            let thinking_pct = session::thinking_pct(&message);
            let has_visible_output = message.content.as_deref().is_some_and(|s| !s.is_empty())
                || !message.tool_calls.is_empty();
            for warning in warnings {
                events.send(AgentEvent::Warned { message: warning }).await?;
            }
            // A blank turn is no news: skip it on abort, and skip a blank
            // truncated turn too, so a provider that cuts before the first
            // token leaves no empty assistant entry behind.
            if !(cut::is_blank(&message) && (outcome.aborted || short.is_some())) {
                self.push_entry(message.clone())?;
            }

            // Non-streaming: the whole text arrives at once, so one chunk.
            // (The streaming path emitted its deltas during the read.)
            // Thinking plays the same one-event shape, and lands first —
            // that is also the arrival order on the wire.
            if !self.stream {
                if let Some(thinking) = message.reasoning.as_ref().filter(|t| !t.is_empty()) {
                    events
                        .send(AgentEvent::Thinking {
                            delta: thinking.clone(),
                        })
                        .await?;
                }
                if let Some(text) = message.content.as_ref().filter(|t| !t.is_empty()) {
                    events
                        .send(AgentEvent::AssistantText {
                            delta: text.clone(),
                        })
                        .await?;
                }
            }

            // A stream cut or a `length` finish means what arrived is a
            // PREFIX of an answer, not an answer: keep it (it is already
            // persisted and, on the streaming path, already on screen) and
            // ask for the rest — bounded, visible, and never by running the
            // partial's tool calls.
            if let Some(cut) = short {
                let turn = cut::ShortTurn {
                    number: turns,
                    request: &messages,
                    telemetry: cut::Telemetry {
                        usage: turn_usage,
                        timings: timings.clone(),
                        thinking_pct,
                    },
                };
                let tool_call_ids = message
                    .tool_calls
                    .iter()
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>();
                match self
                    .handle_cut(
                        cut,
                        &tool_call_ids,
                        &mut continuations,
                        turn,
                        has_visible_output,
                        events,
                        &mut noop_compaction_warned,
                    )
                    .await?
                {
                    // `handle_cut` already sent the TurnEnded carrying the
                    // stop reason that names the truncation.
                    cut::After::Continue => continue,
                    cut::After::EndRun => break,
                    cut::After::Aborted => {
                        // The loop's abort shape finishes the run with every
                        // entry it has.
                        self.emit_abort(events).await?;
                        return Ok(());
                    }
                }
            }

            if message.tool_calls.is_empty() {
                // §17: a final turn with no content is a provider-shape
                // oddity — visible, never silent (the answer would print
                // empty with no explanation).
                if message.content.as_deref().unwrap_or("").is_empty() {
                    events
                        .send(AgentEvent::Warned {
                            message: "assistant response had no content".into(),
                        })
                        .await?;
                }
                events
                    .send(AgentEvent::TurnEnded {
                        stop_reason: "stop".into(),
                        usage: turn_usage,
                        timings,
                        thinking_pct,
                    })
                    .await?;
                break;
            }

            if self
                .execute_tool_batch(&message.tool_calls, session_id.as_deref(), &cancel, events)
                .await?
            {
                return Ok(());
            }

            // The batch finished with the user asking to stop: every call
            // already has its result, so the session is replay-safe — but
            // honoring the stop here spares a whole extra request the user
            // has already disowned.
            if self.abort.load(Ordering::Acquire) {
                self.emit_abort(events).await?;
                return Ok(());
            }

            events
                .send(AgentEvent::TurnEnded {
                    stop_reason: "tool_calls".into(),
                    usage: turn_usage,
                    timings,
                    thinking_pct,
                })
                .await?;

            if let Some(limit) = self.max_turns
                && turns >= limit
            {
                events
                    .send(AgentEvent::Warned {
                        message: format!("reached max_turns ({limit}); stopping run"),
                    })
                    .await?;
                break;
            }

            // §14: the compaction check sits exactly here — after a tool
            // batch, before the next request builds. Context tokens from
            // the last usage (chars/4 estimate when the server reports
            // none); the whole action degrades to a `Warned` on failure.
            let context_tokens = compaction::context_tokens_of(&self.entries)
                .unwrap_or_else(|| compaction::estimate_wire_tokens(&messages));
            if compaction::should_compact(
                context_tokens,
                self.context_window,
                self.compact_threshold_tokens,
            ) {
                self.maybe_compact(None, events, &mut noop_compaction_warned)
                    .await?;
                if self.abort.load(Ordering::Acquire) {
                    self.emit_abort(events).await?;
                    return Ok(());
                }
            }
        }

        // §22: run_end hooks observe a clean finish only — the abort paths
        // above returned without reaching here. Warnings precede RunEnded
        // so every interface sees them inside the run.
        for warning in self
            .hooks
            .run_end(turns, session_id.as_deref(), &self.cwd, &self.abort)
            .await
        {
            events.send(AgentEvent::Warned { message: warning }).await?;
        }
        events.send(AgentEvent::RunEnded { turns }).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
