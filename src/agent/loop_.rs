//! The turn engine: request → tool calls → results → repeat (spec §9).
//!
//! Straight-line async on purpose. Tool failures are values the model can
//! read; only transport and session errors propagate. There is no turn cap
//! — a runaway loop is stopped by the user via the abort flag. The one
//! exception is the continuation budget ([`cut`]): a truncated answer earns
//! a bounded number of "keep going" turns, then the run ends cleanly.

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;
use crate::llm::stream::StreamOutcome;
use crate::llm::types::ChatMessage;

use super::Agent;
use super::compaction;
use super::hooks::PreOutcome;
use super::session::{self, Role};
use super::tools::{Cancel, ToolResult};

mod cut;
mod record;

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
        // The tool-facing view of that same flag: one handle for every call
        // this run dispatches, so a running tool can notice the stop request
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
        // Continuations this run has spent on a truncated answer (§ cut).
        let mut continuations = 0u32;
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
            let outcome = if self.stream {
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
                // The non-streaming path is complete by construction: the
                // whole body arrived in one parse.
                StreamOutcome {
                    response: self
                        .client
                        .send(
                            &self.model,
                            &messages,
                            self.max_tokens,
                            &self.tools.declarations(),
                        )
                        .await?,
                    complete: true,
                }
            };
            // Read before `from_response` consumes the response: the wire
            // type stays the single place finish reasons exist.
            let truncated = outcome
                .response
                .choices
                .first()
                .and_then(|c| c.finish_reason.as_deref())
                == Some("length");
            // A `length` finish explains itself even when the stream ended
            // cleanly, so it outranks the cut; either way the answer is a
            // prefix and the turn is short.
            let short = if truncated {
                Some(cut::Cut::MaxTokens)
            } else if !outcome.complete {
                Some(cut::Cut::StreamCut)
            } else {
                None
            };
            let (message, warnings) = session::from_response(outcome.response)?;
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

            // GAP 2 + GAP 11: a stream cut or a `length` finish means what
            // arrived is a PREFIX of an answer, not an answer. Keep it (it
            // is already persisted and, on the streaming path, already on
            // the user's screen) and ask for the rest — bounded, visible,
            // and never by replaying the partial's tool calls.
            if let Some(cut) = short {
                match self
                    .handle_cut(
                        cut,
                        &message
                            .tool_calls
                            .iter()
                            .map(|c| c.id.clone())
                            .collect::<Vec<_>>(),
                        &mut continuations,
                        &messages,
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
                        // The continuation was a request the user had
                        // already disowned; the loop's abort shape finishes
                        // the run with every entry it has.
                        self.push_message(Role::User, Some("[aborted by user]".into()))?;
                        events
                            .send(AgentEvent::Warned {
                                message: "run aborted".into(),
                            })
                            .await?;
                        events.send(AgentEvent::RunAborted).await?;
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
                // §22: pre_tool hooks sit between Started and dispatch. A
                // call whose provider arguments failed to parse skips them:
                // dispatch's args error is the answer, not a policy call.
                let pre = if call.args_error.is_none() {
                    self.hooks
                        .pre_tool(call, session_id.as_deref(), &self.cwd, &self.abort)
                        .await
                } else {
                    PreOutcome::Allow {
                        warnings: Vec::new(),
                    }
                };
                let (result, executed) = match pre {
                    PreOutcome::Allow { warnings } => {
                        for warning in warnings {
                            events.send(AgentEvent::Warned { message: warning }).await?;
                        }
                        // Invalid provider arguments are never an execution:
                        // dispatch records the parse error and post hooks stay
                        // away, like both tool events (§22).
                        //
                        // dispatch is also the gate for a stop request: the
                        // flag is already in effect there, so a stopped run
                        // never starts another tool.
                        let result = self.tools.dispatch(call, &cancel).await;
                        // A call the user stopped while it ran is not an
                        // execution a `post_tool` hook should observe — the
                        // same rule as a call that never executed. The tool
                        // result shape cannot say "cancelled", so the loop
                        // decides from the flag it owns.
                        let executed =
                            call.args_error.is_none() && !self.abort.load(Ordering::Relaxed);
                        (result, executed)
                    }
                    PreOutcome::Deny { reason, warnings } => {
                        for warning in warnings {
                            events.send(AgentEvent::Warned { message: warning }).await?;
                        }
                        (
                            ToolResult::err(format!("denied by pre_tool hook: {reason}")),
                            false,
                        )
                    }
                    PreOutcome::Interrupted => {
                        (ToolResult::err("aborted before execution".into()), false)
                    }
                };
                self.push_tool_result(&call.id, &result)?;
                events
                    .send(AgentEvent::ToolCallFinished {
                        name: call.name.clone(),
                        id: call.id.clone(),
                        output: result.output.clone(),
                        is_error: result.is_error,
                    })
                    .await?;
                // The result is recorded before post hooks run, so a slow
                // formatter cannot change what the model was told.
                if executed {
                    for warning in self
                        .hooks
                        .post_tool(call, &result, session_id.as_deref(), &self.cwd, &self.abort)
                        .await
                    {
                        events.send(AgentEvent::Warned { message: warning }).await?;
                    }
                }
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
