//! `task`: run a nested `Agent` in-process and return its final answer as
//! the tool result. Replaces delegating through `bash` + `gg -p`, which
//! inherits bash's timeout and process-group kill.
//!
//! The tool is thin: it wraps the child's events in
//! [`crate::events::AgentEvent::Subtask`] tagged with its own call id, and
//! hands the request to a [`SubagentRunner`]. `AgentRunner` is the
//! in-process runner; tests drive the tool with a fake.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use super::ctx::{BoxFuture, Cancel, ToolCtx};
use super::{Tool, ToolResult, parse_args};
use crate::events::AgentEvent;

/// A task request: what the parent asks a subagent to do.
pub struct TaskRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub max_turns: Option<u32>,
}

/// Narrows the requested subagent tool allowlist to the parent run's
/// resolved allowlist: a subagent never gets a tool the parent cannot use.
/// `None` inherits the parent list; a name outside it fails the call. `task`
/// is always rejected, so subagents stay one level deep. Duplicate names
/// collapse to their first occurrence.
pub fn narrow_tools(
    requested: Option<Vec<String>>,
    parent: &[String],
) -> anyhow::Result<Vec<String>> {
    let Some(requested) = requested else {
        return Ok(parent.to_vec());
    };
    for name in &requested {
        if name == "task" {
            anyhow::bail!("subagents cannot spawn subagents (task is one level deep)");
        }
        if !parent.iter().any(|p| p == name) {
            anyhow::bail!("subagent tool {name} is not in the run's tool allowlist");
        }
    }
    let mut out = Vec::new();
    for name in requested {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

/// The turn budget a subagent runs with: the run's `max_turns` is a ceiling,
/// so a request can only lower it. A request of zero is rejected: it would
/// otherwise be silently dropped and lift the run's cap.
pub fn effective_max_turns(
    requested: Option<u32>,
    run: Option<u32>,
) -> anyhow::Result<Option<u32>> {
    if requested == Some(0) {
        anyhow::bail!("max_turns must be at least 1");
    }
    Ok(match (requested, run) {
        (Some(r), Some(cap)) => Some(r.min(cap)),
        (Some(r), None) => Some(r),
        (None, _) => run,
    })
}

/// What a finished subagent reports back: its final answer plus the run's
/// accounting.
pub struct TaskOutcome {
    pub text: String,
    pub turns: u32,
    pub session_id: Option<String>,
    pub usage_in: u64,
    pub usage_out: u64,
}

/// Runs one subagent for a [`TaskRequest`], forwarding the child's events
/// through `events` as they arrive. In-process; one level deep.
pub trait SubagentRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        req: TaskRequest,
        events: tokio::sync::mpsc::Sender<crate::events::AgentEvent>,
    ) -> BoxFuture<'a, anyhow::Result<TaskOutcome>>;
}

/// Builds the nested [`crate::agent::Agent`] a [`TaskRequest`] runs on.
pub trait SubagentFactory: Send + Sync {
    fn build(&self, req: &TaskRequest) -> anyhow::Result<crate::agent::Agent>;
}

/// The in-process runner: builds the agent through the factory, runs it on
/// a tokio task, and forwards its events to the caller's sender.
pub struct AgentRunner {
    factory: Arc<dyn SubagentFactory>,
}

impl AgentRunner {
    pub fn new(factory: Arc<dyn SubagentFactory>) -> Self {
        Self { factory }
    }
}

impl SubagentRunner for AgentRunner {
    fn run<'a>(
        &'a self,
        req: TaskRequest,
        events: tokio::sync::mpsc::Sender<crate::events::AgentEvent>,
    ) -> BoxFuture<'a, anyhow::Result<TaskOutcome>> {
        Box::pin(async move {
            let mut agent = self.factory.build(&req)?;
            let (mut child_tx, mut child_rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
            // The child reports its result (and itself) over a one-shot
            // when `run` finishes; its events stream over `child_tx` while
            // it works.
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let handle = tokio::task::spawn(async move {
                // The child shares the parent's stop flag: `run_nested`
                // honors a stop latched before the child started instead
                // of erasing the parent's abort request.
                let result = agent.run_nested(req.prompt, &mut child_tx).await;
                let _ = done_tx.send((result, agent));
            });
            // Dropping this future (the user stopped the parent run)
            // aborts the child task instead of leaking a detached agent.
            // A finished task aborts nothing, so the clean path costs a
            // no-op.
            let _guard = AbortOnDrop(handle.abort_handle());
            // Forward the child's events to the caller. A closed caller
            // stops sending but keeps draining, so the child's `run`
            // never blocks on a full channel. The stream ends when the
            // child task ends (clean finish, panic, or the guard's
            // abort): its sender is dropped with the task.
            while let Some(event) = child_rx.recv().await {
                let _ = events.send(event).await;
            }
            let (result, agent) = done_rx
                .await
                .map_err(|_| anyhow::anyhow!("subagent task panicked"))?;
            if let Err(e) = result {
                return Err(e.context("subagent run failed"));
            }
            // The task body has finished: its handle joins at once.
            let _ = handle.await;
            outcome_of(&agent)
        })
    }
}

/// Aborts the child task when dropped, so a stopped parent (the tool
/// future dropped) does not leak a running subagent. Dropping the
/// `AbortHandle` of a finished task is a no-op.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Read the run's accounting from the finished agent's entries: the last
/// assistant answer (error if there is none), the turn count, and the
/// token totals.
fn outcome_of(agent: &crate::agent::Agent) -> anyhow::Result<TaskOutcome> {
    use crate::agent::session::{Entry, Role};

    let mut text: Option<String> = None;
    let mut turns = 0u32;
    let mut usage_in = 0u64;
    let mut usage_out = 0u64;
    for entry in agent.entries().iter().rev() {
        if let Entry::Message { message, .. } = entry
            && matches!(message.role, Role::Assistant)
        {
            if message.content.as_deref().is_some_and(|c| !c.is_empty()) && text.is_none() {
                text = message.content.clone();
            }
            turns += 1;
            if let Some(usage) = &message.usage {
                usage_in += u64::from(usage.prompt_tokens);
                usage_out += u64::from(usage.completion_tokens);
            }
        }
    }
    let text = text
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow::anyhow!("subagent produced no final answer"))?;
    Ok(TaskOutcome {
        text,
        turns,
        session_id: agent.store_id().map(str::to_string),
        usage_in,
        usage_out,
    })
}

/// The `task` tool: one nested agent per call.
pub struct TaskTool {
    runner: Arc<dyn SubagentRunner>,
}

#[derive(Deserialize)]
struct TaskArgs {
    prompt: Option<String>,
    model: Option<String>,
    tools: Option<Vec<String>>,
    max_turns: Option<u32>,
}

/// The answer cap: a subagent's result is one tool result, sized like the
/// other tools'.
const MAX_OUTPUT_BYTES: usize = 10 * 1024;
/// Marker appended when the answer is cut at [`MAX_OUTPUT_BYTES`].
const TRUNCATION_MARKER: &str = "... [truncated]";

impl TaskTool {
    pub fn new(runner: Arc<dyn SubagentRunner>) -> Self {
        Self { runner }
    }

    fn cap_text(text: String) -> String {
        if text.len() <= MAX_OUTPUT_BYTES {
            return text;
        }
        // Cut on a char boundary within the marker budget, then mark the cut.
        let budget = MAX_OUTPUT_BYTES - TRUNCATION_MARKER.len();
        let mut cut = String::new();
        for ch in text.chars() {
            if cut.len() + ch.len_utf8() > budget {
                break;
            }
            cut.push(ch);
        }
        cut.push_str(TRUNCATION_MARKER);
        cut
    }
}

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        "Run a subagent: a fresh-context agent with its own session and its \
         own tool registry, in-process. The subagent does the work and the \
         tool returns only its final answer (plus a one-line usage summary). \
         Use it to delegate a bounded piece of work; it is one level deep \
         (subagents cannot spawn subagents) and shares the run's stop \
         request."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "What the subagent should do, self-contained: it starts with a fresh context"},
                "model": {"type": "string", "description": "Optional provider/model to run the subagent on (default: the run's model)"},
                "tools": {"type": "array", "items": {"type": "string"}, "description": "Optional tool allowlist for the subagent; a subset of the run's tool set only (default: the run's tool set)"},
                "max_turns": {"type": "integer", "minimum": 1, "description": "Optional turn budget for the subagent; can only lower the run's max_turns, never raise it (default: the run's max_turns)"}
            },
            "required": ["prompt"]
        })
    }

    /// A subagent may write: `task` is not read-only, so it is never
    /// batched into a concurrent read-only run.
    fn is_read_only(&self) -> bool {
        false
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        // The old trait surface: a context with no events and no call id,
        // so tests can drive the tool directly. The dispatch paths use
        // `execute_with` with the run's ctx.
        Box::pin(async move {
            let ctx = ToolCtx::none(cancel);
            self.execute_with(args, &ctx).await
        })
    }

    fn execute_with<'a>(&'a self, args: &'a Value, ctx: &'a ToolCtx) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: TaskArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let Some(prompt) = args.prompt.filter(|p| !p.trim().is_empty()) else {
                return ToolResult::err("invalid arguments: prompt must not be empty".into());
            };
            if args.max_turns == Some(0) {
                return ToolResult::err("invalid arguments: max_turns must be at least 1".into());
            }
            let req = TaskRequest {
                prompt,
                model: args.model,
                tools: args.tools,
                max_turns: args.max_turns,
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
            // Forward the child's events into the parent's stream, tagged
            // with this call's id. The forwarder runs inline with the
            // runner (not on a detached task), so the tool result returns
            // only after the last event is in the parent's buffer. If the
            // parent channel closes, keep draining and drop: the child must
            // never block on a full channel, and a closed parent still lets
            // the run finish.
            let forward = async move {
                let parent = ctx.events.clone();
                let call_id = ctx.call_id.clone();
                let mut open = parent.is_some();
                while let Some(event) = rx.recv().await {
                    if !open {
                        continue;
                    }
                    if let Some(parent) = &parent
                        && parent
                            .send(AgentEvent::Subtask {
                                task_id: call_id.clone(),
                                event: Box::new(event),
                            })
                            .await
                            .is_err()
                    {
                        open = false;
                    }
                }
            };
            let (outcome, _) = tokio::join!(self.runner.run(req, tx), forward);
            if ctx.cancel.is_cancelled() {
                return ToolResult::err("aborted by user".into());
            }
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(e) => return ToolResult::err(format!("subagent failed: {e:#}")),
            };
            let mut text = Self::cap_text(outcome.text);
            let summary = match outcome.session_id {
                Some(id) => format!(
                    "[subagent: {} turns, {} in / {} out, session {}]",
                    outcome.turns, outcome.usage_in, outcome.usage_out, id
                ),
                None => format!(
                    "[subagent: {} turns, {} in / {} out]",
                    outcome.turns, outcome.usage_in, outcome.usage_out
                ),
            };
            text.push_str("\n\n");
            text.push_str(&summary);
            ToolResult::ok(text)
        })
    }
}

#[cfg(test)]
mod tests;
