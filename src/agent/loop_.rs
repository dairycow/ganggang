//! The turn engine: request → tool calls → results → repeat (spec §9).
//!
//! Straight-line async on purpose. Tool failures are values the model can
//! read; only transport and session errors propagate. There is no turn cap
//! — a runaway loop is stopped by the user via the abort flag.

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

use super::Agent;
use super::compaction;
use super::session::{self, Entry, Message, Role, last_entry_id};
use super::tools::ToolResult;

impl Agent {
    /// Run one user input to completion: turn 1 asks the model, tool calls
    /// are dispatched sequentially, each batch loops back into a new turn
    /// until the model answers without wanting tools.
    pub async fn run(
        &mut self,
        input: String,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        // Every run starts with a clean stop-request state: a flag latched
        // by an earlier run (or an interface path that forgot to reset it)
        // would phantom-abort this one between tool calls.
        self.abort.store(false, Ordering::Relaxed);
        // Set when a batch reports trigger-true/NothingToDo, so the
        // "past the line, no cut point" warning fires once, not per batch.
        let mut noop_compaction_warned = false;
        self.push_message(Role::User, Some(input))?;
        events.send(AgentEvent::RunStarted).await?;
        for warning in std::mem::take(&mut self.skill_warnings)
            .into_iter()
            .chain(std::mem::take(&mut self.agents_warnings))
        {
            events.send(AgentEvent::Warned { message: warning }).await?;
        }

        let mut turns = 0u32;
        loop {
            turns += 1;
            events.send(AgentEvent::TurnStarted).await?;

            // The system prompt is rebuilt per request (spec §12: cheap,
            // no invalidation bugs); the session entries replay after it.
            let system = self.system_prompt();
            // The single most useful debugging affordance a harness can
            // have (§12): dump the first assembled prompt on demand.
            if turns == 1 && std::env::var("GG_DEBUG_PROMPT").is_ok_and(|v| v == "1") {
                eprintln!("{system}");
            }
            let mut messages = vec![ChatMessage {
                role: "system".to_string(),
                content: Some(system),
                ..ChatMessage::default()
            }];
            messages.extend(session::to_wire_with(&self.entries, self.replay_reasoning));
            let response = if self.stream {
                // Deltas ride the stream read: `AssistantText` events land
                // as tokens arrive, with the callback's boxed future giving
                // the event channel real backpressure. A closed consumer
                // resurfaces at the next required send below, which the
                // run propagates with `?` — deltas themselves are
                // best-effort by design.
                let event_tx = events.clone();
                self.client
                    .send_stream(
                        &self.model,
                        &messages,
                        self.max_tokens,
                        &self.tools.declarations(),
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
                    )
                    .await?
            } else {
                self.client
                    .send(
                        &self.model,
                        &messages,
                        self.max_tokens,
                        &self.tools.declarations(),
                    )
                    .await?
            };
            // Read before `from_response` consumes the response: the wire
            // type stays the single place finish reasons exist.
            let truncated = response
                .choices
                .first()
                .and_then(|c| c.finish_reason.as_deref())
                == Some("length");
            let (message, warnings) = session::from_response(response)?;
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
                    })
                    .await?;
                break;
            }

            for (index, call) in message.tool_calls.iter().enumerate() {
                // The one guard the spec allows between tool calls: the
                // user's stop request. Everything before it stays recorded.
                if self.abort.load(Ordering::Relaxed) {
                    // Calls that will never run still need tool_results —
                    // a replayed request with an unanswered tool_call is
                    // rejected by strict providers (§8: a call is never
                    // dropped, so its answer must exist too).
                    for unanswered in &message.tool_calls[index..] {
                        self.push_tool_result(
                            &unanswered.id,
                            &ToolResult {
                                output: "aborted before execution".into(),
                                is_error: true,
                            },
                        )?;
                    }
                    self.push_message(Role::User, Some("[aborted by user]".into()))?;
                    events
                        .send(AgentEvent::Warned {
                            message: "run aborted".into(),
                        })
                        .await?;
                    events.send(AgentEvent::RunAborted).await?;
                    return Ok(());
                }

                events
                    .send(AgentEvent::ToolCallStarted {
                        name: call.name.clone(),
                        id: call.id.clone(),
                        args: call.args.clone(),
                    })
                    .await?;
                let result = self.tools.dispatch(call).await;
                self.push_tool_result(&call.id, &result)?;
                events
                    .send(AgentEvent::ToolCallFinished {
                        name: call.name.clone(),
                        id: call.id.clone(),
                        output: result.output.clone(),
                        is_error: result.is_error,
                    })
                    .await?;
            }

            // The batch finished with the user asking to stop: every call
            // already has its result, so the session is replay-safe — but
            // honoring the stop here spares a whole extra request the user
            // has already disowned. (Checked between calls above for the
            // mid-batch case, which synthesizes the missing results.)
            if self.abort.load(Ordering::Relaxed) {
                self.push_message(Role::User, Some("[aborted by user]".into()))?;
                events
                    .send(AgentEvent::Warned {
                        message: "run aborted".into(),
                    })
                    .await?;
                events.send(AgentEvent::RunAborted).await?;
                return Ok(());
            }

            events
                .send(AgentEvent::TurnEnded {
                    stop_reason: "tool_calls".into(),
                })
                .await?;

            // §14: the compaction check sits exactly here — after a tool
            // batch, before the next request builds. Context tokens from
            // the last usage (chars/4 estimate when the server reports
            // none); the whole action degrades to a `Warned` on failure.
            let context_tokens = compaction::context_tokens_of(&self.entries)
                .unwrap_or_else(|| compaction::estimate_wire_tokens(&messages));
            if compaction::should_compact(context_tokens, self.context_window) {
                self.maybe_compact(None, events, &mut noop_compaction_warned)
                    .await?;
            }
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
