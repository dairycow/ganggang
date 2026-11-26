//! Tool-batch execution for the agent loop: sequential by default, with
//! concurrent dispatch for contiguous runs of 2+ read-only calls. Either
//! way every call is handed the run's stop request, and a stop mid-batch
//! ends the run with the abort shape (the remaining calls get synthesized
//! error results, `RunAborted` is emitted).

use std::sync::atomic::Ordering;

use tokio::sync::mpsc::Sender;

use crate::events::AgentEvent;

use super::super::Agent;
use super::super::hooks::PreOutcome;
use super::super::session::ToolCall;
use super::super::tools::{Cancel, ToolCtx, ToolResult};

enum PreparedCall {
    Runnable,
    Immediate { result: ToolResult, executed: bool },
}

impl Agent {
    /// Execute one assistant turn's `tool_calls`, returning `Ok(true)` if
    /// the run was aborted (all remaining calls get synthesized error
    /// results and `RunAborted` is emitted).
    pub(super) async fn execute_tool_batch(
        &mut self,
        tool_calls: &[ToolCall],
        session_id: Option<&str>,
        cancel: &Cancel,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<bool> {
        let mut cursor = 0usize;
        while cursor < tool_calls.len() {
            let run_ro = self.tools.is_read_only(&tool_calls[cursor].name);
            let mut end = cursor + 1;
            if run_ro {
                while end < tool_calls.len() && self.tools.is_read_only(&tool_calls[end].name) {
                    end += 1;
                }
            }
            if run_ro && end - cursor >= 2 {
                if self
                    .execute_parallel_run(tool_calls, cursor, end, session_id, cancel, events)
                    .await?
                {
                    return Ok(true);
                }
            } else if self
                .execute_sequential_run(tool_calls, cursor, end, session_id, cancel, events)
                .await?
            {
                return Ok(true);
            }
            cursor = end;
        }
        Ok(false)
    }

    async fn abort_remaining(
        &mut self,
        remaining: &[ToolCall],
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        for unanswered in remaining {
            self.push_tool_result(
                &unanswered.id,
                &ToolResult {
                    output: "aborted before execution".into(),
                    is_error: true,
                },
            )?;
        }
        self.emit_abort(events).await
    }

    async fn finalize_aborted_prepared(
        &mut self,
        started: &[ToolCall],
        prepared: Vec<PreparedCall>,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<()> {
        for (call, prep) in started.iter().zip(prepared) {
            let result = match prep {
                PreparedCall::Runnable => ToolResult::err("aborted before execution".into()),
                PreparedCall::Immediate { result, .. } => result,
            };
            self.push_tool_result(&call.id, &result)?;
            events
                .send(AgentEvent::ToolCallFinished {
                    name: call.name.clone(),
                    id: call.id.clone(),
                    output: result.output,
                    is_error: result.is_error,
                })
                .await?;
        }
        Ok(())
    }

    async fn execute_sequential_run(
        &mut self,
        all_calls: &[ToolCall],
        start: usize,
        end: usize,
        session_id: Option<&str>,
        cancel: &Cancel,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<bool> {
        for (offset, call) in all_calls[start..end].iter().enumerate() {
            let global_idx = start + offset;
            if self.abort.load(Ordering::Acquire) {
                self.abort_remaining(&all_calls[global_idx..], events)
                    .await?;
                return Ok(true);
            }
            events
                .send(AgentEvent::ToolCallStarted {
                    name: call.name.clone(),
                    id: call.id.clone(),
                    args: call.args.clone(),
                })
                .await?;
            let pre = if call.args_error.is_none() {
                self.hooks
                    .pre_tool(call, session_id, &self.cwd, &self.abort)
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
                    // A call the user stopped while it ran is not an
                    // execution a `post_tool` hook should observe — the
                    // same rule as a call that never executed. The ctx
                    // carries the run's event stream, so a tool that
                    // observes the run (task) sees it.
                    let ctx = ToolCtx {
                        cancel: cancel.clone(),
                        events: Some(events.clone()),
                        call_id: call.id.clone(),
                    };
                    let result = self.tools.dispatch_with(call, &ctx).await;
                    let executed = call.args_error.is_none() && !self.abort.load(Ordering::Acquire);
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
                    let result = ToolResult::err("aborted before execution".into());
                    self.push_tool_result(&call.id, &result)?;
                    events
                        .send(AgentEvent::ToolCallFinished {
                            name: call.name.clone(),
                            id: call.id.clone(),
                            output: result.output,
                            is_error: result.is_error,
                        })
                        .await?;
                    self.abort_remaining(&all_calls[global_idx + 1..], events)
                        .await?;
                    return Ok(true);
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
            if executed && !self.abort.load(Ordering::Acquire) {
                for warning in self
                    .hooks
                    .post_tool(call, &result, session_id, &self.cwd, &self.abort)
                    .await
                {
                    events.send(AgentEvent::Warned { message: warning }).await?;
                }
            }
        }
        Ok(false)
    }

    async fn execute_parallel_run(
        &mut self,
        all_calls: &[ToolCall],
        start: usize,
        end: usize,
        session_id: Option<&str>,
        cancel: &Cancel,
        events: &mut Sender<AgentEvent>,
    ) -> anyhow::Result<bool> {
        if self.abort.load(Ordering::Acquire) {
            self.abort_remaining(&all_calls[start..], events).await?;
            return Ok(true);
        }

        let slice = &all_calls[start..end];
        let mut prepared = Vec::with_capacity(slice.len());
        let mut runnable_calls = Vec::new();
        for (offset, call) in slice.iter().enumerate() {
            if self.abort.load(Ordering::Acquire) {
                self.finalize_aborted_prepared(&slice[..offset], prepared, events)
                    .await?;
                self.abort_remaining(&all_calls[start + offset..], events)
                    .await?;
                return Ok(true);
            }
            events
                .send(AgentEvent::ToolCallStarted {
                    name: call.name.clone(),
                    id: call.id.clone(),
                    args: call.args.clone(),
                })
                .await?;
            let pre = if call.args_error.is_none() {
                self.hooks
                    .pre_tool(call, session_id, &self.cwd, &self.abort)
                    .await
            } else {
                PreOutcome::Allow {
                    warnings: Vec::new(),
                }
            };
            match pre {
                PreOutcome::Allow { warnings } => {
                    for warning in warnings {
                        events.send(AgentEvent::Warned { message: warning }).await?;
                    }
                    if call.args_error.is_none() {
                        prepared.push(PreparedCall::Runnable);
                        runnable_calls.push(call.clone());
                    } else {
                        prepared.push(PreparedCall::Immediate {
                            result: self
                                .tools
                                .dispatch_with(
                                    call,
                                    &ToolCtx {
                                        cancel: cancel.clone(),
                                        events: Some(events.clone()),
                                        call_id: call.id.clone(),
                                    },
                                )
                                .await,
                            executed: false,
                        });
                    }
                }
                PreOutcome::Deny { reason, warnings } => {
                    for warning in warnings {
                        events.send(AgentEvent::Warned { message: warning }).await?;
                    }
                    prepared.push(PreparedCall::Immediate {
                        result: ToolResult::err(format!("denied by pre_tool hook: {reason}")),
                        executed: false,
                    });
                }
                PreOutcome::Interrupted => {
                    prepared.push(PreparedCall::Immediate {
                        result: ToolResult::err("aborted before execution".into()),
                        executed: false,
                    });
                    self.finalize_aborted_prepared(&slice[..=offset], prepared, events)
                        .await?;
                    self.abort_remaining(&all_calls[start + offset + 1..], events)
                        .await?;
                    return Ok(true);
                }
            }
        }

        if self.abort.load(Ordering::Acquire) {
            self.finalize_aborted_prepared(slice, prepared, events)
                .await?;
            self.abort_remaining(&all_calls[end..], events).await?;
            return Ok(true);
        }

        let parallel_vec = if runnable_calls.is_empty() {
            Vec::new()
        } else {
            // One ctx per call (its own id) is built inside the spawned
            // tasks; this base ctx carries the shared stop request and
            // event sender.
            let ctx = ToolCtx {
                cancel: cancel.clone(),
                events: Some(events.clone()),
                call_id: String::new(),
            };
            let fut = self.tools.dispatch_parallel_with(&runnable_calls, &ctx);
            tokio::select! {
                biased;
                _ = super::wait_for_abort(&self.abort) => {
                    self.finalize_aborted_prepared(slice, prepared, events).await?;
                    self.abort_remaining(&all_calls[end..], events).await?;
                    return Ok(true);
                }
                res = fut => res,
            }
        };
        let mut parallel_results = parallel_vec.into_iter();
        for (call, prep) in slice.iter().zip(prepared) {
            let (result, executed) = match prep {
                PreparedCall::Runnable => (
                    parallel_results
                        .next()
                        .unwrap_or_else(|| ToolResult::err("missing parallel tool result".into())),
                    true,
                ),
                PreparedCall::Immediate { result, executed } => (result, executed),
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
            if executed && !self.abort.load(Ordering::Acquire) {
                for warning in self
                    .hooks
                    .post_tool(call, &result, session_id, &self.cwd, &self.abort)
                    .await
                {
                    events.send(AgentEvent::Warned { message: warning }).await?;
                }
            }
        }
        Ok(false)
    }
}
