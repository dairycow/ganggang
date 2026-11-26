//! End-to-end script for the in-process `AgentRunner`: the factory hands a
//! real agent to the fake server, the child runs through `run_nested`, and
//! the outcome is read back from the finished agent's entries.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::event_values;
use super::fake::{FakeServer, text_response};
use crate::agent::Agent;
use crate::agent::hooks::Hooks;
use crate::agent::tools::Registry;
use crate::agent::tools::task::{AgentRunner, SubagentFactory, SubagentRunner, TaskRequest};
use crate::events::AgentEvent;
use crate::llm::client::LlmClient;

/// The factory a runner test drives the real loop with: a plain client
/// against the fake server, no tools, no session, and the shared stop flag.
struct FakeFactory {
    base_url: String,
    abort: Arc<AtomicBool>,
}

impl SubagentFactory for FakeFactory {
    fn build(&self, req: &TaskRequest) -> anyhow::Result<Agent> {
        Ok(Agent::new(
            LlmClient::new(&self.base_url)?,
            Registry::new(),
            "fake-model".into(),
            512,
            self.abort.clone(),
            std::env::temp_dir(),
            None,
            false,
            131_072,
            Hooks::default(),
        )
        .with_max_turns(req.max_turns))
    }
}

/// Everything buffered in the caller's channel: the runner forwards every
/// child event before it returns, so a drain after the run is complete.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn the_runner_runs_the_subagent_and_reads_back_the_outcome() {
    let server = FakeServer::spawn(vec![text_response("the answer")]);
    let runner = AgentRunner::new(Arc::new(FakeFactory {
        base_url: server.base_url.clone(),
        abort: Arc::new(AtomicBool::new(false)),
    }));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let outcome = runner
        .run(
            TaskRequest {
                prompt: "do it".into(),
                model: None,
                tools: None,
                max_turns: Some(3),
            },
            tx,
        )
        .await
        .unwrap();

    assert_eq!(outcome.text, "the answer");
    assert_eq!(outcome.turns, 1);
    assert_eq!(outcome.usage_in, 10);
    assert_eq!(outcome.usage_out, 5);
    assert_eq!(outcome.session_id, None);

    let values = event_values(&drain(&mut rx));
    assert!(
        values.contains(&json!({"type": "run_started"})),
        "{values:?}"
    );
    assert!(
        values.contains(&json!({"type": "assistant_text", "delta": "the answer"})),
        "{values:?}"
    );
    assert!(
        values.contains(&json!({"type": "run_ended", "turns": 1})),
        "{values:?}"
    );

    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

/// A subagent that answers with no content is a runner error, not an empty
/// result: the loop keeps the blank turn and finishes cleanly, so
/// `outcome_of` is what surfaces the missing answer.
#[tokio::test]
async fn the_runner_errors_when_the_subagent_produces_no_final_answer() {
    let server = FakeServer::spawn(vec![text_response("")]);
    let runner = AgentRunner::new(Arc::new(FakeFactory {
        base_url: server.base_url.clone(),
        abort: Arc::new(AtomicBool::new(false)),
    }));

    let (tx, _rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
    let Err(err) = runner
        .run(
            TaskRequest {
                prompt: "do it".into(),
                model: None,
                tools: None,
                max_turns: Some(3),
            },
            tx,
        )
        .await
    else {
        panic!("the runner must error on an empty final answer");
    };

    assert!(format!("{err:#}").contains("no final answer"), "{err:#}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
