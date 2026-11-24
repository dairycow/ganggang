//! What the loop does with a turn that fell short (GAP 2 / GAP 11): keep
//! the partial answer, ask for the rest, and stop when the budget says so.
//!
//! Two shapes of short turn that mean "the answer is a prefix": the SSE
//! body ended before its sentinel (`StreamOutcome::complete == false`), or
//! the provider says the answer hit `max_tokens`
//! (`finish_reason: "length"`). Either way the next request asks for the
//! remainder instead of throwing the turn away.
//!
//! [`Cut::Aborted`] is the third shape and the odd one out (GAP 3's
//! in-flight half): the turn fell short because the user stopped it. The
//! partial is recorded exactly like the others — its tool calls answered,
//! so the session stays replay-safe — but the run ends there: no
//! truncation warning, no turn marker, no continuation request the user has
//! already disowned. Folding it in here rather than in the loop keeps one
//! abort path, not two.
//!
//! [`handle_cut`] is the loop's half: entries appended in replay-safe order
//! (persist-before-memory, §13), one visible warning per short turn (§17),
//! and the §14 compaction check a continuation request earns. The loop
//! keeps only the branch.
//!
//! A continuation is a turn like any other, so the run's turn budget
//! (`--max-turns`) is spent on it too, and a short answer at the budget's
//! end is not continued: the cap would be exceeded by the very request
//! that asks for the rest.

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::agent::Agent;
use crate::agent::compaction;
use crate::agent::session::{self, Message, Role};
use crate::agent::tools::ToolResult;
use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

/// What a turn's `TurnEnded` carries, whichever kind of turn it was. A
/// short turn reports its usage and timings like a complete one — the
/// footer's context figure and the token totals would otherwise skip it.
#[derive(Debug, Clone, Default)]
pub(super) struct Telemetry {
    pub usage: Option<session::Usage>,
    pub timings: Option<crate::llm::types::Timings>,
    pub thinking_pct: Option<u8>,
}

/// What the loop knows about the turn that fell short.
pub(super) struct ShortTurn<'a> {
    /// The turn's number, one-based: continuations count.
    pub number: u32,
    /// The request the turn answered, for the compaction estimate.
    pub request: &'a [ChatMessage],
    /// What its `TurnEnded` carries.
    pub telemetry: Telemetry,
}

/// How many times one run may ask the model to continue a short answer.
/// Bounded deliberately: a server that hands back one token per turn
/// would otherwise loop until the user kills it.
pub(super) const MAX_CONTINUATIONS: u32 = 3;

/// The user-role instruction that carries a partial answer forward. It is
/// explicit about not repeating, because the usual failure mode of a bare
/// "continue" is the model restating what it already wrote.
pub(super) const CONTINUE_INSTRUCTION: &str =
    "[continue exactly where you stopped; do not repeat text you already wrote]";

/// Why this turn fell short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cut {
    /// The stream ended without `[DONE]` / `message_stop`.
    StreamEnded,
    /// The provider's finish reason was `length`.
    MaxTokens,
    /// The user stopped the run mid-request. The partial is kept like any
    /// other short turn's, but nothing is asked for afterwards: the run is
    /// over, not merely short.
    Aborted,
}

impl Cut {
    /// The `TurnEnded` stop reason for a short turn — distinct from
    /// `"stop"`, so an event consumer can tell a finished answer from a
    /// truncated one (§17: every truncation is visible). Interfaces match
    /// on these: see [`crate::printers`], which accumulates a continued
    /// answer instead of letting the last turn replace it.
    ///
    /// [`Cut::Aborted`] has none: a run the user stopped gets no turn
    /// marker at all, the rule every other abort path follows (§9).
    pub(super) fn stop_reason(self) -> Option<&'static str> {
        match self {
            Self::StreamEnded => Some(crate::events::STOP_STREAM_CUT),
            Self::MaxTokens => Some(crate::events::STOP_LENGTH),
            Self::Aborted => None,
        }
    }
}

/// The decision for one short turn: the warning the user reads, and
/// whether to spend a continuation on it. One warning per short turn —
/// §17's truncation warning, extended with what the loop is doing about
/// it, never a second copy of the same news.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) warning: String,
    pub(super) continue_turn: bool,
}

/// `used` is how many continuations this run has already spent;
/// `at_turn_limit` says the run's turn budget is spent, in which case
/// nothing is continued whatever the continuation budget says.
fn plan(cut: Cut, used: u32, max_tokens: u32, at_turn_limit: bool) -> Plan {
    let head = match cut {
        Cut::StreamEnded => "stream ended early".to_string(),
        Cut::MaxTokens => format!("response truncated by max_tokens={max_tokens}"),
        // A stopped run is not a truncation: the `[aborted by user]` entry
        // and the `run aborted` warning already say what happened, and it
        // earns no continuation — that would replay text the user stopped.
        Cut::Aborted => {
            return Plan {
                warning: String::new(),
                continue_turn: false,
            };
        }
    };
    if at_turn_limit {
        Plan {
            warning: format!("{head}; reached max_turns — the partial answer stands"),
            continue_turn: false,
        }
    } else if used < MAX_CONTINUATIONS {
        Plan {
            warning: format!(
                "{head}; continuing from the partial answer ({}/{MAX_CONTINUATIONS})",
                used + 1
            ),
            continue_turn: true,
        }
    } else {
        Plan {
            warning: format!(
                "{head}; stopping after {used} continuation(s) — the partial answer stands"
            ),
            continue_turn: false,
        }
    }
}

/// What the loop should do once a short turn is recorded and reported.
pub(super) enum After {
    /// The continuation message is appended; ask again.
    Continue,
    /// Budget spent: end the run with the partial answer standing.
    EndRun,
    /// The user had already asked to stop.
    Aborted,
}

/// The answer a truncated tool call gets: recorded so the session stays
/// replay-safe (§8) while the call itself is never dispatched.
const NOT_EXECUTED: &str = "not executed: response was truncated";

/// The same shape for a stop request: the call rides an answer the user
/// stopped, so it never dispatches — and it is answered with the words the
/// loop already uses for a call skipped between tool calls.
const NOT_EXECUTED_ABORT: &str = "aborted before execution";

/// Whether an aborted turn's assistant message is worth an entry at all:
/// a stop request that lands before the first token leaves nothing in any
/// field, and a blank assistant entry in the session and the transcript is
/// news nobody asked for.
pub(super) fn is_blank(message: &Message) -> bool {
    message.content.as_deref().unwrap_or("").is_empty()
        && message.reasoning.as_deref().unwrap_or("").is_empty()
        && message.tool_calls.is_empty()
}

impl Agent {
    /// Record a short turn and decide what follows. The assistant message
    /// is already an entry; its tool calls are the part that must not run
    /// — arguments can be cut mid-JSON and the answer they belong to is
    /// unfinished — yet each still gets a result, so the replayed session
    /// stays valid (§8: a call is never dropped, so its answer must exist
    /// too).
    ///
    /// `used` counts the continuations this run has already spent, and
    /// `turn` says which turn this is and what its `TurnEnded` carries. A
    /// [`Cut::Aborted`] turn skips the warning and the turn marker and
    /// answers its calls with the loop's usual "aborted before execution";
    /// the caller's abort shape (marker entry, `run aborted`, `RunAborted`)
    /// finishes the run.
    pub(super) async fn handle_cut(
        &mut self,
        cut: Cut,
        tool_call_ids: &[String],
        used: &mut u32,
        turn: ShortTurn<'_>,
        events: &mut Sender<AgentEvent>,
        noop_compaction_warned: &mut bool,
    ) -> anyhow::Result<After> {
        for id in tool_call_ids {
            let answer = if cut == Cut::Aborted {
                NOT_EXECUTED_ABORT
            } else {
                NOT_EXECUTED
            };
            self.push_tool_result(id, &ToolResult::err(answer.into()))?;
        }
        // A turn at the run's turn budget is not continued, whatever the
        // continuation budget says.
        let at_turn_limit = self.max_turns.is_some_and(|limit| turn.number >= limit);
        // A truncation warns about itself, then decides on a continuation;
        // an abort warns about nothing (its own marker covers it) and never
        // continues. Empty warning means nothing to say.
        let decision = plan(cut, *used, self.max_tokens, at_turn_limit);
        // A continuation is another request, so it falls under the loop's
        // one guard: the user's stop request — checked before the warning,
        // so a run the user has already disowned is not said to be
        // continuing. Everything the short turn left behind is already an
        // entry, so stopping here stays replay-safe, and no turn marker
        // follows (§9).
        if decision.continue_turn && self.abort.load(Ordering::Acquire) {
            return Ok(After::Aborted);
        }
        if !decision.warning.is_empty() {
            events
                .send(AgentEvent::Warned {
                    message: decision.warning,
                })
                .await?;
        }
        // TurnEnded carries the truncation for every interface, and comes
        // before any compaction warning — the same order a tool batch uses.
        if let Some(stop_reason) = cut.stop_reason() {
            let Telemetry {
                usage,
                timings,
                thinking_pct,
            } = turn.telemetry;
            events
                .send(AgentEvent::TurnEnded {
                    stop_reason: stop_reason.to_string(),
                    usage,
                    timings,
                    thinking_pct,
                })
                .await?;
        }
        if !decision.continue_turn {
            return Ok(if cut == Cut::Aborted {
                After::Aborted
            } else {
                After::EndRun
            });
        }
        *used += 1;
        self.push_message(Role::User, Some(CONTINUE_INSTRUCTION.into()))?;
        // §14: the same check a tool batch earns — the continuation request
        // is about to build on top of the entries just appended.
        let context_tokens = compaction::context_tokens_of(&self.entries)
            .unwrap_or_else(|| compaction::estimate_wire_tokens(turn.request));
        if compaction::should_compact(
            context_tokens,
            self.context_window,
            self.compact_threshold_tokens,
        ) {
            self.maybe_compact(None, events, noop_compaction_warned)
                .await?;
            // A summary the user stopped leaves the run stopped: no
            // request is built on top of it.
            if self.abort.load(Ordering::Acquire) {
                return Ok(After::Aborted);
            }
        }
        Ok(After::Continue)
    }
}
