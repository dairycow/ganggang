//! What the loop does with a turn that fell short (GAP 2 / GAP 11): keep
//! the partial answer, ask for the rest, and stop when the budget says so.
//!
//! Two shapes of short turn, same meaning: the SSE body ended before its
//! sentinel (`StreamOutcome::complete == false`), or the provider says the
//! answer hit `max_tokens` (`finish_reason: "length"`). Either way what
//! arrived is a *prefix* of an answer, so the next request asks for the
//! remainder instead of throwing the turn away.
//!
//! [`handle_cut`] is the loop's half: entries appended in replay-safe order
//! (persist-before-memory, §13), one visible warning per short turn (§17),
//! and the §14 compaction check a continuation request earns. The loop
//! keeps only the branch.

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::agent::Agent;
use crate::agent::compaction;
use crate::agent::session::Role;
use crate::agent::tools::ToolResult;
use crate::events::AgentEvent;
use crate::llm::types::ChatMessage;

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
    StreamCut,
    /// The provider's finish reason was `length`.
    MaxTokens,
}

impl Cut {
    /// The `TurnEnded` stop reason for a short turn — distinct from
    /// `"stop"`, so an event consumer can tell a finished answer from a
    /// truncated one (§17: every truncation is visible). Interfaces match
    /// on these: see [`crate::printers`], which accumulates a continued
    /// answer instead of letting the last turn replace it.
    pub(super) fn stop_reason(self) -> &'static str {
        match self {
            Self::StreamCut => crate::events::STOP_STREAM_CUT,
            Self::MaxTokens => crate::events::STOP_LENGTH,
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

/// `used` is how many continuations this run has already spent.
fn plan(cut: Cut, used: u32, max_tokens: u32) -> Plan {
    let head = match cut {
        Cut::StreamCut => "stream ended early".to_string(),
        Cut::MaxTokens => format!("response truncated by max_tokens={max_tokens}"),
    };
    if used < MAX_CONTINUATIONS {
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

impl Agent {
    /// Record a short turn and decide what follows. The assistant message
    /// is already an entry; its tool calls are the part that must not run
    /// — arguments can be cut mid-JSON and the answer they belong to is
    /// unfinished — yet each still gets a result, so the replayed session
    /// stays valid (§8: a call is never dropped, so its answer must exist
    /// too).
    ///
    /// `used` counts the continuations this run has already spent.
    pub(super) async fn handle_cut(
        &mut self,
        cut: Cut,
        tool_call_ids: &[String],
        used: &mut u32,
        request_messages: &[ChatMessage],
        events: &mut Sender<AgentEvent>,
        noop_compaction_warned: &mut bool,
    ) -> anyhow::Result<After> {
        for id in tool_call_ids {
            self.push_tool_result(id, &ToolResult::err(NOT_EXECUTED.into()))?;
        }
        let decision = plan(cut, *used, self.max_tokens);
        events
            .send(AgentEvent::Warned {
                message: decision.warning,
            })
            .await?;
        // A continuation is another request, so it falls under the loop's
        // one guard: the user's stop request. Checked before TurnEnded so an
        // aborted cut looks like every other abort (§9: no turn marker on a
        // run the user stopped). Everything the short turn left behind is
        // already an entry, so stopping here stays replay-safe.
        if decision.continue_turn && self.abort.load(Ordering::Relaxed) {
            return Ok(After::Aborted);
        }
        // TurnEnded carries the truncation for every interface, and comes
        // before any compaction warning — the same order a tool batch uses.
        let stop_reason = cut.stop_reason().to_string();
        events.send(AgentEvent::TurnEnded { stop_reason }).await?;
        if !decision.continue_turn {
            return Ok(After::EndRun);
        }
        *used += 1;
        self.push_message(Role::User, Some(CONTINUE_INSTRUCTION.into()))?;
        // §14: the same check a tool batch earns — the continuation request
        // is about to build on top of the entries just appended.
        let context_tokens = compaction::context_tokens_of(&self.entries)
            .unwrap_or_else(|| compaction::estimate_wire_tokens(request_messages));
        if compaction::should_compact(context_tokens, self.context_window) {
            self.maybe_compact(None, events, noop_compaction_warned)
                .await?;
        }
        Ok(After::Continue)
    }
}
