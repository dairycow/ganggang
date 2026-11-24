//! JSON mode (spec §15, M6): the session header record, then one
//! `AgentEvent` per line on stdout — the same enum the TUI consumes, so
//! the two interfaces cannot drift. Stderr stays for diagnostics.

use std::io::Write;

use anyhow::Context;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::events::AgentEvent;

/// Render one event as its JSON line. `to_string` on the serde enum is
/// infallible in practice; the error context names the event type if a
/// Value ever stops serializing.
pub fn event_line(event: &AgentEvent) -> anyhow::Result<String> {
    serde_json::to_string(event).context("serializing AgentEvent")
}

/// Run one prompt, streaming the header record then every event to
/// stdout, flushed per line so pipe consumers see progress as it
/// happens. The answer is in the events (`assistant_text` deltas);
/// stdout carries nothing else. A consumer closing the pipe early
/// (`… | head -3`) is a normal end for a stream producer, not a
/// failure: the run stops quietly.
pub async fn run_json(mut agent: Agent, prompt: String) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();
    match agent.session_header_record() {
        Ok(Some(header)) => {
            // A write that already fails here means the consumer left
            // before the stream started; the events would fail too.
            if writeln!(stdout, "{header}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                return Ok(());
            }
        }
        // No session (`--no-session`): nothing to echo — not a warning.
        Ok(None) => {}
        // An unreadable record is a diagnostic, not a failed run: the
        // events themselves are the interface (§15).
        Err(e) => eprintln!("warning: no session header: {e:#}"),
    }

    // The drain task mirrors print mode's: without it, a run longer
    // than the channel capacity would deadlock the agent on send.
    let (mut tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let drain = tokio::spawn(async move {
        let mut stdout = std::io::stdout();
        while let Some(event) = rx.recv().await {
            match event_line(&event) {
                Ok(line) => {
                    if writeln!(stdout, "{line}")
                        .and_then(|_| stdout.flush())
                        .is_err()
                    {
                        // Broken pipe: the consumer is gone. Stop
                        // draining; the run unwinds via the closed
                        // channel and the flag below quiets it.
                        return true;
                    }
                }
                Err(e) => eprintln!("warning: {e:#}"),
            }
        }
        false
    });

    let result = agent.run(prompt, &mut tx).await;
    drop(tx);
    let pipe_broke = drain.await.context("event renderer task panicked")?;
    if pipe_broke {
        return Ok(());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden event stream: the scripted tool-call → answer run,
    /// one JSON line per event, byte-pinned (field order included).
    #[test]
    fn scripted_run_matches_the_golden_event_stream() {
        let raw = include_str!("../fixtures/events_golden.jsonl");
        let events: Vec<AgentEvent> = vec![
            AgentEvent::RunStarted,
            AgentEvent::TurnStarted,
            AgentEvent::ToolCallStarted {
                name: "echo".into(),
                id: "call_1".into(),
                args: serde_json::json!({"text": "hi"}),
            },
            AgentEvent::ToolCallFinished {
                name: "echo".into(),
                id: "call_1".into(),
                output: "hi".into(),
                is_error: false,
            },
            AgentEvent::TurnEnded {
                stop_reason: "tool_calls".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText {
                delta: "done".into(),
            },
            AgentEvent::TurnEnded {
                stop_reason: "stop".into(),
                usage: None,
                timings: None,
                thinking_pct: None,
            },
            AgentEvent::RunEnded { turns: 2 },
        ];
        let rendered: Vec<String> = events.iter().map(event_line).map(Result::unwrap).collect();
        let golden: Vec<&str> = raw.lines().collect();
        assert_eq!(rendered, golden);
    }
}
