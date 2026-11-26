//! The agent side of the TUI: the task that owns the [`Agent`], executes
//! submitted inputs as runs, forwards the event stream to the UI, and
//! reports outcomes with usage totals.

use tokio::sync::mpsc;

use std::sync::Arc;

use crate::agent::Agent;
use crate::agent::session::{Entry, Message, Role};
use crate::events::AgentEvent;

/// What the agent task tells the UI, beyond the agent events themselves.
/// Usage lives on entries, not events — the footer's totals arrive here.
#[derive(Debug)]
pub enum UiEvent {
    Agent(AgentEvent),
    RunComplete {
        tokens_in: u64,
        tokens_out: u64,
        /// Prompt+completion tokens of the LAST assistant entry — the
        /// footer's context % (§15) is this against the context window.
        context_tokens: u64,
        entries: usize,
    },
    RunFailed {
        error: String,
    },
    /// A `!`/`!!` shell line finished; if `to_context`, its output is
    /// fed to the model as the next user message (pi's semantics).
    ShellDone {
        command: String,
        output: String,
        exit_code: Option<i32>,
        to_context: bool,
    },
    /// Reply to `/context`: the exact system prompt the next request
    /// carries (§12's other debugging affordance).
    SystemPrompt(String),
    /// Reply to `/compact` (§14): one line about what the compaction
    /// did — including "nothing to compact" and failure, which the run
    /// survives — plus a fresh context-token estimate for the footer
    /// (the real figure only exists after the next request).
    Compacted {
        message: String,
        context_tokens: u64,
    },
    ModelSwitched {
        label: String,
        context_window: u32,
        reasoning_effort: Option<String>,
        reasoning_budget_tokens: Option<u32>,
    },
    /// Reply to `/reload`: the re-discovered skill set (shared with the
    /// suggestion popup) and the counts the feedback line reports.
    Reloaded {
        agents_count: usize,
        skills: Arc<Vec<crate::agent::skills::Skill>>,
        warnings: Vec<String>,
    },
    /// A background local-models probe finished: the refreshed rows for
    /// the `/model` popup, replacing the catalog seed (and any earlier
    /// discovery result).
    ModelsDiscovered(Vec<super::suggest::ModelChoice>),
    /// A bare `/resume`'s session-store scan finished, off the loop:
    /// the picker's rows and the scan's warnings. The event loop takes
    /// this before [`apply`] sees it (it opens the picker from it), so
    /// [`apply`] only names it to stay exhaustive.
    SessionsListed {
        sessions: Vec<crate::agent::session::io::SessionFile>,
        warnings: Vec<String>,
    },
}

/// What the UI asks the agent task to do. A plain prompt runs; anything
/// else is a query the task answers with a [`UiEvent`].
#[derive(Debug)]
pub enum AgentInput {
    Prompt(String),
    DumpContext,
    Compact(Option<String>),
    SwitchEndpoint(crate::config::Endpoint),
    Reload,
}

/// Consume submitted inputs, forward the event stream to the UI, report
/// run outcomes with usage totals. Each run executes on its own task so a
/// panic inside the loop surfaces as a `RunFailed` (JoinError) instead of
/// unwinding this loop and leaving the UI wedged busy forever.
pub(super) async fn agent_task(
    mut agent: Agent,
    mut inputs: mpsc::Receiver<AgentInput>,
    ui: mpsc::Sender<UiEvent>,
) {
    while let Some(input) = inputs.recv().await {
        let input = match input {
            AgentInput::DumpContext => {
                let _ = ui.send(UiEvent::SystemPrompt(agent.system_prompt())).await;
                continue;
            }
            AgentInput::Compact(instructions) => {
                let outcome = match agent.compact_once(instructions.as_deref()).await {
                    Ok(outcome) => outcome,
                    // Session-I/O class failure: the honest report, same
                    // voice a failed run would use.
                    Err(e) => crate::agent::compaction::CompactOutcome::Failed(format!("{e:#}")),
                };
                let message = match outcome {
                    crate::agent::compaction::CompactOutcome::NothingToDo => {
                        "nothing to compact yet".to_string()
                    }
                    crate::agent::compaction::CompactOutcome::Compacted {
                        dropped,
                        first_kept_id,
                    } => format!(
                        "compacted: {dropped} entries summarized (kept from {first_kept_id})"
                    ),
                    crate::agent::compaction::CompactOutcome::Failed(detail) => {
                        format!("compaction failed ({detail}); continuing un-compacted")
                    }
                    crate::agent::compaction::CompactOutcome::Aborted => {
                        "compaction aborted".to_string()
                    }
                };
                // The footer must not keep showing the pre-compaction
                // figure: estimate the summarized view now (chars/4, the
                // trigger's own fallback), replaced by real usage on the
                // next request.
                let context_tokens = crate::agent::compaction::estimate_wire_tokens(
                    &crate::agent::session::to_wire_with(agent.entries(), agent.replay_reasoning()),
                ) as u64;
                let _ = ui
                    .send(UiEvent::Compacted {
                        message,
                        context_tokens,
                    })
                    .await;
                continue;
            }
            AgentInput::SwitchEndpoint(endpoint) => {
                match agent.switch_endpoint(&endpoint) {
                    Ok(()) => {
                        let _ = ui
                            .send(UiEvent::ModelSwitched {
                                label: endpoint.label(),
                                context_window: endpoint.context_window,
                                reasoning_effort: endpoint.reasoning_effort.clone(),
                                reasoning_budget_tokens: endpoint.reasoning_budget_tokens,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = ui
                            .send(UiEvent::Agent(AgentEvent::Warned {
                                message: format!("model switch failed: {e:#}"),
                            }))
                            .await;
                    }
                }
                continue;
            }
            AgentInput::Reload => {
                let (agents_count, _, warnings) = agent.reload_context();
                let _ = ui
                    .send(UiEvent::Reloaded {
                        agents_count,
                        skills: Arc::new(agent.skills().to_vec()),
                        warnings,
                    })
                    .await;
                continue;
            }
            AgentInput::Prompt(input) => input,
        };
        let (mut tx, mut rx) = mpsc::channel::<AgentEvent>(64);
        let forward_tx = ui.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if forward_tx.send(UiEvent::Agent(event)).await.is_err() {
                    break;
                }
            }
        });
        let run = tokio::spawn(async move {
            let result = agent.run(input, &mut tx).await;
            (agent, result)
        });
        let joined = run.await;
        let _ = forward.await;
        match joined {
            Ok((agent_back, Ok(()))) => {
                agent = agent_back;
                let (tokens_in, tokens_out) = usage_totals(agent.entries());
                let context_tokens = last_context_tokens(agent.entries());
                let entries = agent.entries().len();
                let _ = ui
                    .send(UiEvent::RunComplete {
                        tokens_in,
                        tokens_out,
                        context_tokens,
                        entries,
                    })
                    .await;
            }
            Ok((agent_back, Err(e))) => {
                agent = agent_back;
                let _ = ui
                    .send(UiEvent::RunFailed {
                        error: format!("{e:#}"),
                    })
                    .await;
            }
            Err(join_error) => {
                // The agent itself is gone (panicked with it moved into
                // the run task). Report, then end the task: the UI's next
                // send fails and it exits cleanly with the terminal whole.
                let _ = ui
                    .send(UiEvent::RunFailed {
                        error: format!("agent task panicked: {join_error}"),
                    })
                    .await;
                break;
            }
        }
    }
}

/// Fold one event from the agent task into the view. `false` ends the
/// TUI: the agent task's input channel is gone.
pub(super) async fn apply(
    event: UiEvent,
    state: &mut super::view::State,
    input: &mut mpsc::Sender<AgentInput>,
) -> bool {
    match event {
        UiEvent::Agent(event) => state.apply(&event),
        UiEvent::RunComplete {
            tokens_in,
            tokens_out,
            context_tokens,
            entries,
        } => {
            state.footer.set_usage(tokens_in, tokens_out);
            state.set_context_tokens(context_tokens);
            state.record_totals(tokens_in, tokens_out, entries);
            state.run_finished();
        }
        UiEvent::RunFailed { error } => {
            state.push_failure(&error);
            state.run_finished();
        }
        UiEvent::SystemPrompt(prompt) => {
            state.push_info(&format!("— system prompt —\n{prompt}"));
        }
        UiEvent::Compacted {
            message,
            context_tokens,
        } => {
            state.push_info(&message);
            state.set_context_tokens(context_tokens);
            state.run_finished();
        }
        UiEvent::ModelSwitched {
            label,
            context_window,
            reasoning_effort,
            reasoning_budget_tokens,
        } => {
            state.set_model(&label, context_window);
            state.set_thinking(reasoning_effort.as_deref(), reasoning_budget_tokens);
        }
        UiEvent::Reloaded {
            agents_count,
            skills,
            warnings,
        } => {
            let skill_count = skills.len();
            state.set_skills(skills);
            state.push_info(&format!(
                "reloaded: {agents_count} context file(s), {skill_count} skill(s)"
            ));
            for warning in warnings {
                state.push_warning(&warning);
            }
        }
        UiEvent::ModelsDiscovered(models) => {
            state.set_models(std::sync::Arc::new(models));
        }
        // The loop takes this one before apply sees it: it opens the
        // picker from it itself (commands::sessions_listed).
        UiEvent::SessionsListed { .. } => {}
        UiEvent::ShellDone {
            command,
            output,
            exit_code,
            to_context,
        } => {
            state.push_shell(&command, &output, exit_code, to_context);
            if !to_context {
                state.run_finished();
                return true;
            }
            // The model sees the shell line and its output; the transcript
            // already shows it, so no user block is pushed for the same
            // text.
            state.begin_run();
            let message = format!(
                "[shell] $ {command}\n[exit {}]\n{output}",
                exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "killed by signal".into())
            );
            return input.send(AgentInput::Prompt(message)).await.is_ok();
        }
    }
    true
}

/// Prompt+completion tokens of the newest assistant entry — the request
/// the run just made, i.e. the context the footer's % describes.
pub(super) fn last_context_tokens(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            Entry::Message {
                message:
                    Message {
                        role: Role::Assistant,
                        usage: Some(u),
                        ..
                    },
                ..
            } => Some(u.prompt_tokens as u64 + u.completion_tokens as u64),
            _ => None,
        })
        .unwrap_or(0)
}

/// Session-wide usage totals (§13): summed from assistant entries, so a
/// resumed session starts with the totals of everything before it.
pub(super) fn usage_totals(entries: &[Entry]) -> (u64, u64) {
    let (tokens_in, tokens_out) =
        entries
            .iter()
            .fold((0u64, 0u64), |(tin, tout), entry| match entry {
                // Only assistant entries count: a provider stamping usage
                // on a user message must not inflate the totals.
                Entry::Message { message, .. } if message.role == Role::Assistant => {
                    match &message.usage {
                        Some(u) => (
                            tin + u.prompt_tokens as u64,
                            tout + u.completion_tokens as u64,
                        ),
                        None => (tin, tout),
                    }
                }
                _ => (tin, tout),
            });
    (tokens_in, tokens_out)
}
