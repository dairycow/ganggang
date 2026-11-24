//! The turn engine: request → tool calls → results → repeat (spec §9).
//!
//! Straight-line async on purpose. Tool failures are values the model can
//! read; only transport and session errors propagate. Runaway loops are
//! stopped by the user via the abort flag (Esc / Ctrl+C / SIGTERM) or an
//! optional `--max-turns` / `GG_MAX_TURNS` cap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

use super::Agent;
use super::compaction;
use super::session::{self, Entry, Message, Role, last_entry_id};
use super::tools::ToolResult;

mod dispatch;

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
    pub async fn run(
        &mut self,
        input: String,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        // Every run starts with a clean stop-request state: a flag latched
        // by an earlier run (or an interface path that forgot to reset it)
        // would phantom-abort this one between tool calls.
        self.abort.store(false, Ordering::Release);
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
        let mut wrap_up_nudged = false;
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
            messages.extend(session::to_wire_with(&self.entries, self.replay_reasoning));

            if let Some(limit) = self.max_turns
                && limit >= 2
                && turns >= soft_nudge_turn(limit)
            {
                if !wrap_up_nudged {
                    wrap_up_nudged = true;
                    events
                        .send(AgentEvent::Warned {
                            message: format!(
                                "approaching max_turns ({turns}/{limit}); nudging model to wrap up"
                            ),
                        })
                        .await?;
                }
                let remaining = limit.saturating_sub(turns);
                let notice = if remaining == 0 {
                    format!(
                        "[system notice: this is your final turn ({turns}/{limit}, 0 turn(s) remaining). Do not call any tools; provide your final response now.]"
                    )
                } else {
                    format!(
                        "[system notice: you are on turn {turns} of {limit} ({remaining} turn(s) remaining). Wrap up your work and return your final response soon without starting new exploration.]"
                    )
                };
                if let Some(last) = messages.last_mut().filter(|m| m.role == "tool") {
                    let existing = last.content.take().unwrap_or_default();
                    last.content = Some(if existing.is_empty() {
                        notice
                    } else {
                        format!("{existing}\n\n{notice}")
                    });
                } else {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: Some(notice),
                        ..ChatMessage::default()
                    });
                }
            }

            let declarations = self.tools.declarations();
            let response = if self.stream {
                let event_tx = events.clone();
                let req = self.client.send_stream(
                    &self.model,
                    &messages,
                    self.max_tokens,
                    &declarations,
                    |delta| {
                        let tx = event_tx.clone();
                        Box::pin(async move {
                            let event = match delta {
                                crate::llm::stream::Delta::Text(t) => {
                                    AgentEvent::AssistantText { delta: t }
                                }
                                crate::llm::stream::Delta::Thinking(t) => {
                                    AgentEvent::Thinking { delta: t }
                                }
                            };
                            let _ = tx.send(event).await;
                        })
                    },
                );
                tokio::select! {
                    biased;
                    _ = wait_for_abort(&self.abort) => {
                        self.emit_abort(events).await?;
                        return Ok(());
                    }
                    res = req => res?,
                }
            } else {
                let req = self
                    .client
                    .send(&self.model, &messages, self.max_tokens, &declarations);
                tokio::select! {
                    biased;
                    _ = wait_for_abort(&self.abort) => {
                        self.emit_abort(events).await?;
                        return Ok(());
                    }
                    res = req => res?,
                }
            };
            // Read before `from_response` consumes the response: the wire
            // type stays the single place finish reasons and timings exist.
            let truncated = response
                .choices
                .first()
                .and_then(|c| c.finish_reason.as_deref())
                == Some("length");
            let timings = response.timings.clone();
            let (message, warnings) = session::from_response(response)?;
            let turn_usage = message.usage;
            let thinking_pct = session::thinking_pct(&message);
            // §17: a length finish is a truncation — visible, never silent.
            if truncated {
                events
                    .send(AgentEvent::Warned {
                        message: format!("response truncated by max_tokens={}", self.max_tokens),
                    })
                    .await?;
            }
            for warning in warnings {
                events.send(AgentEvent::Warned { message: warning }).await?;
            }
            self.push_entry(message.clone())?;

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
                .execute_tool_batch(&message.tool_calls, session_id.as_deref(), events)
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

impl Agent {
    /// Entries form a linear chain: each new one names the previous as
    /// parent. The entry is persisted BEFORE it joins the in-memory log —
    /// flush-per-append is the crash-safety contract (§13) — and session
    /// I/O errors abort the run (§17).
    fn push_entry(&mut self, message: Message) -> anyhow::Result<()> {
        let parent = self.entries.last().map(last_entry_id);
        let entry = Entry::Message {
            id: session::new_entry_id(),
            parent_id: parent,
            timestamp: session::now_iso(),
            message,
        };
        if let Some(store) = &mut self.store {
            store.append(&entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    fn push_message(&mut self, role: Role, content: Option<String>) -> anyhow::Result<()> {
        self.push_entry(Message {
            role,
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
            is_error: false,
            reasoning: None,
        })
    }
}

impl Agent {
    fn push_tool_result(&mut self, call_id: &str, result: &ToolResult) -> anyhow::Result<()> {
        self.push_entry(Message {
            role: Role::ToolResult,
            content: Some(result.output.clone()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
            usage: None,
            is_error: result.is_error,
            reasoning: None,
        })
    }
}

#[cfg(test)]
mod tests;
