//! One request to the model, streamed or not, with the run's stop request
//! wired into the streaming read (the in-flight half of GAP 3).
//!
//! The loop asks, this answers with a [`StreamOutcome`], and the loop's
//! short-turn path decides what an aborted turn means. Split out of
//! `run` so the turn engine stays a turn engine: the plumbing that hands
//! deltas to the event channel and turns "the user stopped before the
//! response head" into an outcome belongs here.

use std::future::Future;
use std::pin::Pin;
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
    /// Both paths carry the run's stop request: streaming into the request
    /// and its SSE read, so Esc stops a generation instead of only the
    /// calls after it, and the non-streaming wait as a race. The loop does
    /// not wrap this call in a select of its own — dropping the streaming
    /// future from outside would discard the partial it is about to
    /// return. A stop request that lands before the response head is an
    /// error; here it becomes an aborted, incomplete outcome rather than a
    /// failed run — `run`'s short-turn path then finishes the run with the
    /// normal abort shape.
    ///
    /// `without_reasoning` sends this one request with reasoning switched
    /// off: the retry a turn earns when reasoning used up the whole
    /// `max_tokens`. Every other request carries the endpoint's settings.
    pub(super) async fn ask(
        &self,
        messages: &[ChatMessage],
        events: &Sender<AgentEvent>,
        without_reasoning: bool,
    ) -> anyhow::Result<StreamOutcome> {
        let outcome = self.request(messages, events, without_reasoning).await;
        // Whether it succeeded or not, the retries it took are the run's
        // news — passed on before the loop acts on the answer.
        self.relay_notices(events).await?;
        outcome
    }

    /// Hand the client's retry notices on as warnings. The client keeps
    /// them instead of printing, because one of the interfaces owns the
    /// whole screen and stderr would land in the middle of it; a `Warned`
    /// is a line on stderr in print mode and a line in the transcript in
    /// the TUI.
    pub(in crate::agent) async fn relay_notices(
        &self,
        events: &Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        for notice in self.client.take_notices() {
            events.send(AgentEvent::Warned { message: notice }).await?;
        }
        Ok(())
    }

    /// The request itself: see [`ask`](Self::ask).
    async fn request(
        &self,
        messages: &[ChatMessage],
        events: &Sender<AgentEvent>,
        without_reasoning: bool,
    ) -> anyhow::Result<StreamOutcome> {
        if !self.stream {
            // The non-streaming path is complete by construction: the whole
            // body arrives in one parse. `send` cannot observe the flag
            // mid-body, so the wait for the body is raced against it — a
            // stop request then comes back as an aborted outcome holding
            // nothing, which is the truth: nothing arrived.
            let tools = self.tools.declarations();
            let send = async {
                match without_reasoning {
                    true => {
                        self.client
                            .send_without_reasoning(&self.model, messages, self.max_tokens, &tools)
                            .await
                    }
                    false => {
                        self.client
                            .send(&self.model, messages, self.max_tokens, &tools)
                            .await
                    }
                }
            };
            return tokio::select! {
                biased;
                _ = super::wait_for_abort(&self.abort) => Ok(StreamOutcome {
                    response: empty_response(),
                    complete: false,
                    aborted: true,
                }),
                res = send => res.map(|response| StreamOutcome {
                    response,
                    complete: true,
                    aborted: false,
                }),
            };
        }
        let send = async {
            let (model, max_tokens, tools, abort) = (
                &self.model,
                self.max_tokens,
                self.tools.declarations(),
                &self.abort,
            );
            match without_reasoning {
                true => {
                    self.client
                        .send_stream_without_reasoning(
                            model,
                            messages,
                            max_tokens,
                            &tools,
                            abort,
                            forward_deltas(events),
                        )
                        .await
                }
                false => {
                    self.client
                        .send_stream(
                            model,
                            messages,
                            max_tokens,
                            &tools,
                            abort,
                            forward_deltas(events),
                        )
                        .await
                }
            }
        };
        match send.await {
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

/// The streaming path's delta forwarder: each text or thinking delta
/// becomes the matching event on the channel, with the send's boxed future
/// giving the channel real backpressure.
fn forward_deltas(
    events: &Sender<AgentEvent>,
) -> impl FnMut(Delta) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    let event_tx = events.clone();
    move |delta| {
        let tx = event_tx.clone();
        Box::pin(async move {
            let event = match delta {
                Delta::Text(t) => AgentEvent::AssistantText { delta: t },
                Delta::Thinking(t) => AgentEvent::Thinking { delta: t },
            };
            let _ = tx.send(event).await;
        })
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
        timings: None,
    }
}
