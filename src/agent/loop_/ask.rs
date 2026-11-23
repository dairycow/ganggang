//! One request to the model, streamed or not, with the run's stop request
//! wired into the streaming read (the in-flight half of GAP 3).
//!
//! The loop asks, this answers with a [`StreamOutcome`], and the loop's
//! short-turn path decides what an aborted turn means. Split out of
//! `run` so the turn engine stays a turn engine: the plumbing that hands
//! deltas to the event channel and turns "the user stopped before the
//! response head" into an outcome belongs here.

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::agent::Agent;
use crate::events::AgentEvent;
use crate::llm::stream::{Delta, StreamOutcome};
use crate::llm::types::{ChatMessage, ChatResponse, Choice};

impl Agent {
    /// Ask the model for one turn. The streaming path delivers text and
    /// thinking deltas as they arrive — the callback's boxed future gives
    /// the event channel real backpressure, and a closed consumer
    /// resurfaces at the next required send in `run`, which the run
    /// propagates with `?` (deltas themselves are best-effort by design).
    ///
    /// Streaming also carries the run's stop request into the request and
    /// its SSE read, so Esc stops a generation instead of only the calls
    /// after it. A stop request that lands before the response head is an
    /// error; here it becomes an aborted, incomplete outcome rather than a
    /// failed run — `run`'s short-turn path then finishes the run with the
    /// normal abort shape.
    pub(super) async fn ask(
        &self,
        messages: &[ChatMessage],
        events: &Sender<AgentEvent>,
    ) -> anyhow::Result<StreamOutcome> {
        if !self.stream {
            // The non-streaming path is complete by construction: the whole
            // body arrived in one parse. It has no in-flight stop path of
            // its own — `send` cannot observe the flag mid-body.
            return Ok(StreamOutcome {
                response: self
                    .client
                    .send(
                        &self.model,
                        messages,
                        self.max_tokens,
                        &self.tools.declarations(),
                    )
                    .await?,
                complete: true,
                aborted: false,
            });
        }
        let event_tx = events.clone();
        let send = self
            .client
            .send_stream(
                &self.model,
                messages,
                self.max_tokens,
                &self.tools.declarations(),
                &self.abort,
                |delta| {
                    let tx = event_tx.clone();
                    Box::pin(async move {
                        let event = match delta {
                            Delta::Text(t) => AgentEvent::AssistantText { delta: t },
                            Delta::Thinking(t) => AgentEvent::Thinking { delta: t },
                        };
                        let _ = tx.send(event).await;
                    })
                },
            )
            .await;
        match send {
            Ok(outcome) => Ok(outcome),
            Err(_err) if self.abort.load(Ordering::Relaxed) => Ok(StreamOutcome {
                // Nothing arrived, so there is nothing to keep. The error
                // text is the same words the loop's abort marker uses; the
                // stop request, not the transport, is the verdict.
                response: empty_response(),
                complete: false,
                aborted: true,
            }),
            Err(err) => Err(err),
        }
    }
}

/// The response shape of a stream that stopped before assembling anything:
/// one choice, nothing in it. [`crate::agent::session::from_response`]
/// needs a choice to read, so an empty `choices` is not an option.
fn empty_response() -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage::default(),
            finish_reason: None,
        }],
        usage: None,
    }
}
