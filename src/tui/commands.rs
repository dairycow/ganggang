//! Submission classification (spec §15): the first character of a
//! submitted line decides what it is — `/` runs a command, `!`/`!!` runs
//! a shell line, anything else goes to the model. Pure, so every rule is
//! unit-tested without a terminal.

mod model;
mod session;
mod think;

/// The event loop calls these two (mod.rs answers
/// `UiEvent::SessionsListed` with `sessions_listed`, and starts the
/// list's scan with `spawn_session_scan` after a draw).
pub(super) use session::{
    copy_session_path, open_session, session_info, sessions_listed, spawn_session_scan,
};

#[cfg(test)]
use think::parse_think_args;

#[cfg(test)]
mod tests;

/// What the user submitted, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// Send this text to the model as the next user message.
    Plain(String),
    Command(Command),
    /// `!cmd` enters the output into context; `!!cmd` shows it locally.
    Shell {
        command: String,
        to_context: bool,
    },
}

/// The fixed command set (spec §15) plus `/compact` (§14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New,
    /// Compact the context now; an argument becomes focus instructions
    /// for the summarize call (§14's manual path).
    Compact(Option<String>),
    /// Show the session's path, model, entries and tokens; an argument
    /// is ignored, as the list's `info` row's is.
    Session,
    /// Dump the assembled system prompt (§12's debugging affordance).
    Context,
    /// List loaded user-level hooks (§22), or say why there are none.
    Hooks,
    Skill {
        name: String,
        args: String,
    },
    /// `/model <provider/model>` switches; the TUI runs the highlighted
    /// row's own command through here. A bare `/model` that reaches the
    /// handler anyway points at the list above the composer.
    Model(Option<String>),
    /// No argument shows the current thinking configuration; an argument
    /// sets the effort and/or budget on the active endpoint.
    Think(Option<String>),
    /// Re-run discovery of context files and skills from disk.
    Reload,
    Quit,
    /// Not a built-in.
    Unknown(String),
}

/// Classify a submitted editor text. Trimmed-empty submissions are the
/// caller's business (they should not reach the model); they classify as
/// Plain and the event loop drops them.
pub fn classify_submission(text: &str) -> Submission {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("!!") {
        let command = rest.trim().to_string();
        if !command.is_empty() {
            return Submission::Shell {
                command,
                to_context: false,
            };
        }
    }
    if let Some(rest) = text.strip_prefix('!') {
        let command = rest.trim().to_string();
        if !command.is_empty() {
            return Submission::Shell {
                command,
                to_context: true,
            };
        }
    }
    if let Some(rest) = text.strip_prefix('/') {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or_default();
        let arg = parts.next().unwrap_or_default().trim().to_string();
        return Submission::Command(match head {
            "new" => Command::New,
            "compact" => Command::Compact(Some(arg)).filter_arg(),
            "session" => Command::Session,
            "context" => Command::Context,
            "model" => Command::Model(Some(arg)).filter_arg(),
            "think" => Command::Think(Some(arg)).filter_arg(),
            "reload" => Command::Reload,
            "hooks" => Command::Hooks,
            "quit" | "exit" => Command::Quit,
            _ if head.starts_with("skill:") => Command::Skill {
                name: head["skill:".len()..].trim().to_string(),
                args: arg,
            },
            _ => Command::Unknown(head.to_string()),
        });
    }
    Submission::Plain(text.to_string())
}

impl Command {
    /// `/compact` with an empty argument is the plain compaction.
    fn filter_arg(self) -> Self {
        match self {
            Command::Compact(Some(arg)) if arg.is_empty() => Command::Compact(None),
            Command::Model(Some(arg)) if arg.is_empty() => Command::Model(None),
            Command::Think(Some(arg)) if arg.is_empty() => Command::Think(None),
            other => other,
        }
    }
}

/// The valid commands, for unknown-command feedback.
pub fn known_commands() -> &'static str {
    "/new /compact [instructions] /session [filter|copy|info] /context /hooks /skill:<name> [args] /model [provider/model] /think [effort|off|default] [budget] /reload /quit"
}

/// Everything a command needs to act: view state, the agent channels, the
/// factory that rebuilds agents, the shared skills, and the working
/// directory. One struct instead of seven parameters.
///
/// `cwd` is the directory gg is working in: where the agent's tools run
/// and where `@mentions` and `!shell` lines resolve. `/session` moves it
/// when it opens a session that started in another directory.
/// `skills` is the view's shared set, taken by value as a snapshot of
/// its `Arc` at command start, so a `/reload` mid-command cannot swap
/// the set out from under the running command.
pub(super) struct CmdCtx<'a> {
    pub state: &'a mut super::view::State,
    pub factory: &'a super::Factory,
    pub input_tx: &'a mut tokio::sync::mpsc::Sender<super::AgentInput>,
    pub ui_tx: &'a tokio::sync::mpsc::Sender<super::agent_task::UiEvent>,
    pub skills: std::sync::Arc<Vec<crate::agent::skills::Skill>>,
    pub hooks: &'a crate::agent::hooks::Hooks,
    pub cwd: &'a mut std::path::PathBuf,
    pub abort: &'a std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub config: &'a crate::config::Config,
    pub active_endpoint: &'a mut crate::config::Endpoint,
}

/// Execute one command; returns Ok(true) when the TUI should exit.
pub(super) async fn handle_command(command: Command, ctx: CmdCtx<'_>) -> anyhow::Result<bool> {
    let CmdCtx {
        state,
        factory,
        input_tx,
        ui_tx,
        skills,
        hooks,
        cwd,
        abort,
        config,
        active_endpoint,
    } = ctx;
    match command {
        Command::Reload => {
            // The agent task re-runs discovery and reports back as
            // UiEvent::Reloaded; the view swaps its skill set there.
            input_tx.send(super::AgentInput::Reload).await?;
            // The `/model` popup's rows refresh in the background from
            // the local servers' `/v1/models` probes.
            super::models::spawn_model_discovery(config.providers.clone(), ui_tx.clone());
        }
        Command::Quit => return Ok(true),
        Command::Hooks => {
            if hooks.disabled() {
                state.push_info("hooks disabled by --no-hooks");
            } else if hooks.is_empty() {
                state.push_info("no hooks configured (~/.gg/hooks.toml)");
            } else {
                state.push_info(&format!("hooks ({}):", hooks.specs().len()));
                for hook in hooks.specs() {
                    let tools = hook
                        .tools
                        .as_ref()
                        .map_or_else(|| "*".to_string(), |tools| tools.join(","));
                    state.push_info(&format!(
                        "  {:<9} [{}] {}s  {}",
                        hook.event.as_str(),
                        tools,
                        hook.timeout_secs,
                        hook.command
                    ));
                }
            }
        }
        Command::Session => {
            let path = state
                .session_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not persisted (--no-session)".into());
            state.push_info(&format!(
                "session: {path}\nmodel: {}\nentries: {}\ntokens: {} in / {} out",
                active_endpoint.label(),
                state.entries,
                state.tokens_in,
                state.tokens_out,
            ));
        }
        Command::Context => {
            // The agent task answers with UiEvent::SystemPrompt — the
            // exact assembly the next request carries, not a rebuild.
            input_tx.send(super::AgentInput::DumpContext).await?;
        }
        Command::Compact(instructions) => {
            // §14's manual path: the agent task runs the same compaction
            // the trigger uses, with the argument as focus instructions.
            // Feedback arrives as UiEvent::Compacted.
            state.begin_run();
            input_tx
                .send(super::AgentInput::Compact(instructions))
                .await?;
        }
        Command::Skill { name, args } => {
            let Some(skill) = skills.iter().find(|s| s.name == name) else {
                state.push_warning(&format!(
                    "no skill named {name:?}; type /skill: to list skills"
                ));
                return Ok(false);
            };
            // pi's forcing semantics: the instructions (plus any args)
            // become the next user message and run immediately. The
            // transcript shows what was typed; `@file` mentions in the
            // args ride along in the payload only — scanned over the
            // args, never the skill's own text, where an `@path` is the
            // skill's writing, not the user's mention.
            let display = if args.is_empty() {
                format!(
                    "Follow this skill's instructions.\n\n{}",
                    skill.instructions
                )
            } else {
                format!(
                    "Follow this skill's instructions for: {args}\n\n{}",
                    skill.instructions
                )
            };
            let payload = match super::mentions::attachment_block(&args, cwd) {
                Some(block) => [display.as_str(), block.as_str()].concat(),
                None => display.clone(),
            };
            state.push_user(&display);
            state.begin_run();
            input_tx.send(super::AgentInput::Prompt(payload)).await?;
        }
        Command::New => {
            let boot = factory(super::SessionPick::New, cwd, active_endpoint, abort.clone())?;
            swap_agent(boot, state, input_tx, ui_tx, cwd);
        }
        Command::Model(choice) => {
            model::handle_model(
                choice,
                CmdCtx {
                    state,
                    factory,
                    input_tx,
                    ui_tx,
                    skills,
                    hooks,
                    cwd,
                    abort,
                    config,
                    active_endpoint,
                },
            )
            .await?;
        }
        Command::Think(arg) => {
            think::handle_think(
                arg,
                CmdCtx {
                    state,
                    factory,
                    input_tx,
                    ui_tx,
                    skills,
                    hooks,
                    cwd,
                    abort,
                    config,
                    active_endpoint,
                },
            )
            .await?;
        }
        Command::Unknown(name) => {
            if name == "skills" {
                state.push_warning("/skills was removed: type /skill: to list skills");
            } else {
                state.push_warning(&format!(
                    "unknown command /{name} — valid: {}",
                    known_commands()
                ));
            }
        }
    }
    Ok(false)
}

/// Point the session at a new agent: fresh input channel, fresh agent
/// task (the old one exits when its channel closes — commands only run
/// while idle, so no run is in flight to orphan), seeded transcript, and
/// the footer showing `directory`, the one the agent runs in.
fn swap_agent(
    boot: super::Boot,
    state: &mut super::view::State,
    input_tx: &mut tokio::sync::mpsc::Sender<super::AgentInput>,
    ui_tx: &tokio::sync::mpsc::Sender<super::agent_task::UiEvent>,
    directory: &std::path::Path,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<super::AgentInput>(16);
    *input_tx = tx;
    state.new_session(boot.session_path, boot.sessions_disabled);
    state.set_directory(directory);
    state.seed_from_entries(boot.agent.entries());
    state.set_skills(std::sync::Arc::new(boot.agent.skills().to_vec()));
    tokio::spawn(super::agent_task::agent_task(boot.agent, rx, ui_tx.clone()));
}
