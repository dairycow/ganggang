//! The turn engine: request → tool calls → results → repeat (spec §9).
//!
//! Straight-line async on purpose. Tool failures are values the model can
//! read; only transport and session errors propagate. Runaway loops are
//! stopped by the user via the abort flag (Esc / Ctrl+C / SIGTERM) or an
//! optional `--max-turns` / `GG_MAX_TURNS` cap. The cap bounds one thing
//! more than it did: a truncated answer earns a few "keep going" turns
//! ([`cut`]), and those count toward it like any other.
//!
//! `--max-seconds` is the same budget on the clock ([`time_budget`]): a
//! nudge at 80% of it, a final request at the end of it, and the run stops
//! when that request's turn is over.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

use super::Agent;
use super::compaction;
use super::session::{self, Role};
use super::tools::{Cancel, ToolResult};

/// The answer a tool call gets on the time budget's final turn.
const TIME_BUDGET_SPENT: &str = "not executed: time budget reached";

mod ask;
mod cut;
mod dispatch;
mod record;
mod time_budget;

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
    /// End the run on the time budget: the warning every interface sees.
    async fn emit_time_stop(&self, events: &mut Sender<AgentEvent>) -> anyhow::Result<()> {
        let Some(limit) = self.max_seconds else {
            return Ok(());
        };
        events
            .send(AgentEvent::Warned {
                message: format!("reached max_seconds ({limit}s); stopping run"),
            })
            .await?;
        Ok(())
    }

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
        // Every run starts with a clean stop-request state: a flag latched
        // by an earlier run (or an interface path that forgot to reset it)
        // would phantom-abort this one between tool calls.
        self.abort.store(false, Ordering::Release);
        // The clock the time budget is read from: one start for the run.
        let started_at = Instant::now();
        self.run_started_at = Some(started_at);
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
        // The time budget's two lines, each crossed once per run: the 80%
        // nudge, then the final request.
        let mut time_soft_nudged = false;
        let mut time_final_turn = false;
        // Wrap-up notices keyed by the entry id they ride: recorded once and
        // replayed from then on, so the request stays append-only past the
        // 80% line and the provider's KV prefix cache survives the nudges.
        // Nothing here is persisted — the session file never sees a notice.
        let mut notices: HashMap<String, String> = HashMap::new();
        // The continuations this run has spent on short answers: back to
        // back, and in total (§ cut).
        let mut continuations = cut::Continuations::default();
        // Thought-only `max_tokens` cuts back to back, each retried with
        // reasoning off. A turn that needed no retry starts the count again.
        let mut consecutive_thought_only = 0u32;
        // Set when a second turn in a row spends the whole `max_tokens` on
        // reasoning: every request after it in this run goes out with
        // reasoning off.
        let mut reasoning_off_for_run = false;
        // Set for the one request that asks a thought-only turn again.
        let mut without_reasoning = false;
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
            // The time budget, read before the request: the 80% line
            // nudges once, the budget itself makes this the final turn.
            let mut time_notice = None;
            if let Some((elapsed, limit)) =
                time_budget::clock(self.seconds_into_run(), self.max_seconds)
            {
                let step = time_budget::step(elapsed, limit, time_soft_nudged, time_final_turn);
                let warning = match step {
                    time_budget::Step::None => None,
                    time_budget::Step::Soft => {
                        time_soft_nudged = true;
                        Some(format!(
                            "approaching max_seconds ({elapsed}s/{limit}s); nudging model to wrap up"
                        ))
                    }
                    time_budget::Step::Final => {
                        time_final_turn = true;
                        Some(format!(
                            "reached max_seconds ({limit}s); asking model for final response"
                        ))
                    }
                };
                if let Some(message) = warning {
                    events.send(AgentEvent::Warned { message }).await?;
                }
                time_notice = time_budget::notice(step, elapsed, limit);
            }
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
            // The turn budget's notice and the time budget's ride together:
            // both are earned at most twice a run and both stay put.
            let mut trailing_notice: Option<String> = None;
            let budget_notice = self
                .max_turns
                .and_then(|limit| wrap_up_notice(turns, limit));
            let budget_notice = match (budget_notice, time_notice) {
                (Some(turns_notice), Some(time_notice)) => {
                    Some(format!("{turns_notice}\n\n{time_notice}"))
                }
                (one, other) => one.or(other),
            };
            if let Some(notice) = budget_notice {
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
            // `without_reasoning` covers one request: the retry of a
            // thought-only turn. Once the fallback has latched, every
            // request goes out with reasoning off instead.
            let retrying = std::mem::take(&mut without_reasoning);
            let reasoning_off = reasoning_off_for_run || retrying;
            let outcome = match self.ask(&messages, events, reasoning_off).await {
                Ok(outcome) => outcome,
                // The server refused the request without reasoning (it takes
                // `reasoning_effort` but not `"none"`): the run ends the
                // way the truncation alone would have ended it, and the
                // warning says what the server answered. Every other
                // failure is the request's own, and fails the run: the
                // error is written into the session first, so the file
                // says why the history stops where it does.
                Err(err) if retrying && crate::llm::client::is_refusal(&err) => {
                    events
                        .send(AgentEvent::Warned {
                            message: format!(
                                "the request with reasoning off was refused; \
                                 stopping instead: {err:#}"
                            ),
                        })
                        .await?;
                    break;
                }
                Err(err) => {
                    let _ = self.push_error(&format!("{err:#}"));
                    return Err(err);
                }
            };
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
            // A `max_tokens` cut that left nothing but reasoning has no
            // partial to continue, and the same request would think its
            // way to the same cut. While reasoning was on for this request
            // and the turn budget is not spent, the turn is asked once more
            // without it: on its own the first time in a row, and for the
            // rest of the run the second time back to back.
            let thought_only = truncated && !has_visible_output && !cut::is_blank(&message);
            let retry_without_reasoning = thought_only
                && !reasoning_off
                && !self.client.reasoning_is_off()
                && !self.at_turn_limit(turns)
                && !time_final_turn;
            // The retry of a thought-only turn is no recovery yet: only a
            // turn that needed no retry ends the run of thought-only cuts.
            if !thought_only && !reasoning_off {
                consecutive_thought_only = 0;
            }
            let thought_only_again = retry_without_reasoning && consecutive_thought_only >= 1;
            // A blank turn is no news: skip it on abort, and skip a blank
            // truncated turn too, so a provider that cuts before the first
            // token leaves no empty assistant entry behind. A turn asked
            // again without reasoning is not kept either: the retry asks
            // the same question, so there is nothing to carry forward.
            if !(cut::is_blank(&message) && (outcome.aborted || short.is_some()))
                && !retry_without_reasoning
            {
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
                // What the request carried as far as reasoning goes. The API
                // decides whether a request can say it at all (`None` on
                // the Anthropic Messages API); then this request must have
                // said it: it was the retry of a thought-only turn, the run
                // has reasoning off, or the endpoint is configured `none`.
                let off_field = self.client.without_reasoning_field();
                let turn = cut::ShortTurn {
                    number: turns,
                    request: &messages,
                    telemetry: cut::Telemetry {
                        usage: turn_usage,
                        timings: timings.clone(),
                        thinking_pct,
                    },
                    reasoning: cut::Reasoning {
                        thought_only,
                        retry: retry_without_reasoning,
                        again: thought_only_again,
                        off: off_field.is_some()
                            && (reasoning_off || self.client.reasoning_is_off()),
                        retrying,
                        off_field,
                    },
                    at_time_limit: time_final_turn,
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
                    // Nothing of the thought-only turn was kept, so the next
                    // request is this one again, now with reasoning off.
                    cut::After::RetryWithoutReasoning => {
                        consecutive_thought_only += 1;
                        reasoning_off_for_run |= thought_only_again;
                        without_reasoning = true;
                        continue;
                    }
                    cut::After::EndRun => {
                        if time_final_turn {
                            self.emit_time_stop(events).await?;
                        }
                        break;
                    }
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
                // The final turn's answer ends the run, budget spent.
                if time_final_turn {
                    self.emit_time_stop(events).await?;
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

            // The final turn asked for tools anyway: none of them run, since
            // the budget is spent, yet each gets a result so the session
            // stays replay-safe.
            if time_final_turn {
                for call in &message.tool_calls {
                    self.push_tool_result(&call.id, &ToolResult::err(TIME_BUDGET_SPENT.into()))?;
                }
                self.emit_time_stop(events).await?;
                events
                    .send(AgentEvent::TurnEnded {
                        stop_reason: "tool_calls".into(),
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
            // A complete tool turn ends any chain of cuts before it: the
            // next truncation starts a new chain with a fresh budget.
            continuations.complete_turn();

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
            // the last usage plus the entries appended since it (chars/4
            // estimate when the server reports none); the whole action
            // degrades to a `Warned` on failure.
            let context_tokens = compaction::context_tokens_of(&self.entries)
                .unwrap_or_else(|| compaction::estimate_wire_tokens(&messages));
            if compaction::should_compact(
                context_tokens,
                self.context_window,
                self.compact_threshold_tokens,
                self.max_tokens,
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
