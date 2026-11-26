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
//! that asks for the rest. The budget has two bounds: [`MAX_CONTINUATIONS`]
//! for cuts back to back, a chain a complete tool turn ends, and
//! [`MAX_RUN_CONTINUATIONS`] for the run as a whole.
//!
//! A `max_tokens` cut that leaves nothing but reasoning has no partial to
//! carry forward, and the same request would think its way to the same
//! cut. While reasoning was on for the request, the loop asks for the turn
//! once more with it off and keeps nothing of the thought-only turn. The
//! first such turn in a row is retried on its own; the turn after it goes
//! out as configured again. A second, back to back with it, switches
//! reasoning off for the rest of the run. A server that refuses the retry
//! ends the run the way the truncation alone would have.

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
    /// The `max_tokens` cut left nothing but reasoning, and the loop can
    /// ask the turn again without it: reasoning was on for the request, and
    /// the run's turn budget is not spent.
    pub thought_only: bool,
    /// [`Self::thought_only`], and the turn before it needed the same
    /// retry: reasoning goes off for the rest of the run.
    pub thought_only_again: bool,
    /// The time budget is spent: this was the final request whatever the
    /// turn budget says.
    pub at_time_limit: bool,
}

/// The budget a short turn ran out of, when one is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spent {
    Turns,
    Seconds,
}

/// How many times one run may ask the model to continue a short answer
/// back to back. Bounded deliberately: a server that hands back one token
/// per turn would otherwise loop until the user kills it. A complete tool
/// turn ends the chain, so the next cut starts with the full budget again.
pub(super) const MAX_CONTINUATIONS: u32 = 3;

/// How many continuations one run may spend in total, however many
/// complete turns sit between them: a long run keeps its budget for each
/// scattered truncation, and a server that truncates on and off all run
/// still stops here.
pub(super) const MAX_RUN_CONTINUATIONS: u32 = 12;

/// The continuation budget of one run, in its two parts.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Continuations {
    /// Continuations spent on cuts back to back, since the last complete
    /// tool turn.
    pub(super) chain: u32,
    /// Every continuation spent in this run.
    pub(super) run: u32,
}

impl Continuations {
    /// Spend one continuation: both counts move together.
    fn spend(&mut self) {
        self.chain += 1;
        self.run += 1;
    }

    /// A complete turn finished its tool batch: the chain of cuts is over.
    pub(super) fn complete_turn(&mut self) {
        self.chain = 0;
    }

    /// The count and scope of the budget that refuses another
    /// continuation, when one does.
    fn exhausted_by(&self) -> Option<(u32, &'static str)> {
        if self.chain >= MAX_CONTINUATIONS {
            Some((self.chain, "in a row"))
        } else if self.run >= MAX_RUN_CONTINUATIONS {
            Some((self.run, "this run"))
        } else {
            None
        }
    }
}

/// The user-role instruction that carries a partial answer forward. It is
/// explicit about not repeating, because the usual failure mode of a bare
/// "continue" is the model restating what it already wrote.
pub(super) const CONTINUE_INSTRUCTION: &str =
    "[continue exactly where you stopped; do not repeat text you already wrote]";

/// The instruction for a cut answer that carried tool calls. Their
/// arguments can be cut mid-JSON, so none of them ran: the model is told
/// to make the call again in pieces it can finish.
pub(super) const CONTINUE_TOOL_INSTRUCTION: &str = "[your previous tool call was cut off by max_tokens and was not executed; retry it in smaller pieces or continue]";

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
    /// The next request asks for this turn again with reasoning off,
    /// instead of the remainder of the partial answer.
    pub(super) without_reasoning: bool,
}

/// `budget` is the run's continuation budget; `at_turn_limit` says the
/// run's turn budget is spent, in which case nothing is continued whatever
/// the continuation budget says. A short turn that produced no visible
/// output is not continued either: there is nothing to carry forward, and
/// asking again would repeat the same cut. The exception is `thought_only`
/// (see [`ShortTurn`]): the same question asked without reasoning has not
/// been tried yet. `again` says the turn before needed that retry too.
fn plan(
    cut: Cut,
    budget: &Continuations,
    max_tokens: u32,
    spent: Option<Spent>,
    has_visible_output: bool,
    thought_only: bool,
    again: bool,
) -> Plan {
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
                without_reasoning: false,
            };
        }
    };
    if !has_visible_output {
        if thought_only && spent.is_none() {
            return Plan {
                warning: if again {
                    format!(
                        "{head} with nothing but reasoning again; asking again with \
                         reasoning off, which stays off for the rest of this run"
                    )
                } else {
                    format!(
                        "{head} with nothing but reasoning; asking again with \
                         reasoning off for this turn"
                    )
                },
                continue_turn: true,
                without_reasoning: true,
            };
        }
        return Plan {
            warning: format!(
                "{head} before producing visible output; stopping instead of continuing"
            ),
            continue_turn: false,
            without_reasoning: false,
        };
    }
    if let Some(spent) = spent {
        let budget = match spent {
            Spent::Turns => "max_turns",
            Spent::Seconds => "max_seconds",
        };
        Plan {
            warning: format!("{head}; reached {budget} — the partial answer stands"),
            continue_turn: false,
            without_reasoning: false,
        }
    } else if let Some((spent, scope)) = budget.exhausted_by() {
        Plan {
            warning: format!(
                "{head}; stopping after {spent} continuation(s) {scope} — the partial answer stands"
            ),
            continue_turn: false,
            without_reasoning: false,
        }
    } else {
        Plan {
            warning: format!(
                "{head}; continuing from the partial answer ({}/{MAX_CONTINUATIONS})",
                budget.chain + 1
            ),
            continue_turn: true,
            without_reasoning: false,
        }
    }
}

/// What the loop should do once a short turn is recorded and reported.
pub(super) enum After {
    /// The continuation message is appended; ask again.
    Continue,
    /// Nothing was appended: ask for the same turn again, with reasoning
    /// off for that request, and for the rest of the run when it is the
    /// second thought-only turn back to back.
    RetryWithoutReasoning,
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
    /// Whether the run's turn budget is spent at `turn`: nothing is asked
    /// for past it, since that request would exceed the cap.
    pub(super) fn at_turn_limit(&self, turn: u32) -> bool {
        self.max_turns.is_some_and(|limit| turn >= limit)
    }

    /// Record a short turn and decide what follows. The assistant message
    /// is already an entry; its tool calls are the part that must not run
    /// — arguments can be cut mid-JSON and the answer they belong to is
    /// unfinished — yet each still gets a result, so the replayed session
    /// stays valid (§8: a call is never dropped, so its answer must exist
    /// too).
    ///
    /// `budget` counts the continuations this run has already spent, and
    /// `turn` says which turn this is and what its `TurnEnded` carries.
    /// `has_visible_output` says whether the short turn left text or a tool
    /// call behind; a blank truncation stops instead of continuing. A
    /// [`Cut::Aborted`] turn skips the warning and the turn marker and
    /// answers its calls with the loop's usual "aborted before execution";
    /// the caller's abort shape (marker entry, `run aborted`, `RunAborted`)
    /// finishes the run. A thought-only truncation the loop can retry
    /// (`turn.thought_only`) is neither continued nor ended: the caller
    /// kept no entry for it, and asks for the same turn again with
    /// reasoning off. A cut `max_tokens` answer that carried tool calls is
    /// continued with [`CONTINUE_TOOL_INSTRUCTION`] instead.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_cut(
        &mut self,
        cut: Cut,
        tool_call_ids: &[String],
        budget: &mut Continuations,
        turn: ShortTurn<'_>,
        has_visible_output: bool,
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
        // A turn at the run's turn budget, or the time budget's final turn,
        // is not continued, whatever the continuation budget says.
        let spent = if turn.at_time_limit {
            Some(Spent::Seconds)
        } else if self.at_turn_limit(turn.number) {
            Some(Spent::Turns)
        } else {
            None
        };
        // A truncation warns about itself, then decides on a continuation;
        // an abort warns about nothing (its own marker covers it) and never
        // continues. Empty warning means nothing to say.
        let decision = plan(
            cut,
            budget,
            self.max_tokens,
            spent,
            has_visible_output,
            turn.thought_only,
            turn.thought_only_again,
        );
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
        if decision.without_reasoning {
            return Ok(After::RetryWithoutReasoning);
        }
        budget.spend();
        let instruction = if cut == Cut::MaxTokens && !tool_call_ids.is_empty() {
            CONTINUE_TOOL_INSTRUCTION
        } else {
            CONTINUE_INSTRUCTION
        };
        self.push_message(Role::User, Some(instruction.into()))?;
        // §14: the same check a tool batch earns — the continuation request
        // is about to build on top of the entries just appended.
        let context_tokens = compaction::context_tokens_of(&self.entries)
            .unwrap_or_else(|| compaction::estimate_wire_tokens(turn.request));
        if compaction::should_compact(
            context_tokens,
            self.context_window,
            self.compact_threshold_tokens,
            self.max_tokens,
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
