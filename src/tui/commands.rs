//! Submission classification (spec §15): the first character of a
//! submitted line decides what it is — `/` runs a command, `!`/`!!` runs
//! a shell line, anything else goes to the model. Pure, so every rule is
//! unit-tested without a terminal.

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
    /// No argument lists sessions; an argument picks by list number or
    /// id prefix.
    Resume(Option<String>),
    /// Compact the context now; an argument becomes focus instructions
    /// for the summarize call (§14's manual path).
    Compact(Option<String>),
    Skills,
    Session,
    /// Dump the assembled system prompt (§12's debugging affordance).
    Context,
    /// List prompt templates (M7.4), mirroring `/skills`.
    Prompts,
    /// List loaded user-level hooks (§22), or say why there are none.
    Hooks,
    Skill {
        name: String,
        args: String,
    },
    /// No argument lists configured providers/models; an argument
    /// switches by list number, provider, model, or `provider/model`.
    Model(Option<String>),
    Quit,
    /// Not a built-in. The name may still match a prompt template
    /// (M7.4), so the args ride along for the insertion.
    Unknown {
        name: String,
        args: String,
    },
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
            "resume" => Command::Resume(Some(arg)).filter_arg(),
            "compact" => Command::Compact(Some(arg)).filter_arg(),
            "skills" => Command::Skills,
            "session" => Command::Session,
            "context" => Command::Context,
            "prompts" => Command::Prompts,
            "model" => Command::Model(Some(arg)).filter_arg(),
            "hooks" => Command::Hooks,
            "quit" | "exit" => Command::Quit,
            _ if head.starts_with("skill:") => Command::Skill {
                name: head["skill:".len()..].trim().to_string(),
                args: arg,
            },
            _ => Command::Unknown {
                name: head.to_string(),
                args: arg,
            },
        });
    }
    Submission::Plain(text.to_string())
}

impl Command {
    /// `/resume` with an empty argument is a bare listing request;
    /// `/compact` with an empty argument is the plain compaction.
    fn filter_arg(self) -> Self {
        match self {
            Command::Resume(Some(arg)) if arg.is_empty() => Command::Resume(None),
            Command::Compact(Some(arg)) if arg.is_empty() => Command::Compact(None),
            Command::Model(Some(arg)) if arg.is_empty() => Command::Model(None),
            other => other,
        }
    }
}

/// The valid commands, for unknown-command feedback.
pub fn known_commands() -> &'static str {
    "/new /resume [n|prefix] /compact [instructions] /skills /session /context /prompts /hooks /skill:<name> [args] /model [provider/model] /quit"
}

/// Everything a command needs to act: view state, the agent channels, the
/// factory that rebuilds agents, the shared skills, and the session cwd.
/// One struct instead of seven parameters.
pub(super) struct CmdCtx<'a> {
    pub state: &'a mut super::view::State,
    pub factory: &'a super::Factory,
    pub input_tx: &'a mut tokio::sync::mpsc::Sender<super::AgentInput>,
    pub ui_tx: &'a tokio::sync::mpsc::Sender<super::agent_task::UiEvent>,
    pub skills: &'a [crate::agent::skills::Skill],
    pub prompts: &'a [crate::prompts::PromptTemplate],
    pub hooks: &'a crate::agent::hooks::Hooks,
    pub cwd: &'a std::path::Path,
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
        prompts,
        hooks,
        cwd,
        abort,
        config,
        active_endpoint,
    } = ctx;
    match command {
        Command::Quit => return Ok(true),
        Command::Skills => {
            if skills.is_empty() {
                state.push_info("no skills discovered");
            }
            for skill in skills {
                state.push_info(&format!(
                    "  {}{} — {}",
                    skill.name,
                    if skill.disable_model_invocation {
                        " (model-invocation disabled)"
                    } else {
                        ""
                    },
                    skill.path.display(),
                ));
            }
        }
        Command::Prompts => {
            if prompts.is_empty() {
                state.push_info("no prompt templates (put ~/.gg/prompts/*.md files here)");
            }
            for template in prompts {
                state.push_info(&format!(
                    "  /{} — {} ({})",
                    template.name,
                    if template.description.is_empty() {
                        "no description"
                    } else {
                        &template.description
                    },
                    template.path.display(),
                ));
            }
        }
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
                    "no skill named {name:?}; /skills lists what exists"
                ));
                return Ok(false);
            };
            // pi's forcing semantics: the instructions (plus any args)
            // become the next user message and run immediately.
            let message = if args.is_empty() {
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
            state.push_user(&message);
            state.begin_run();
            input_tx.send(super::AgentInput::Prompt(message)).await?;
        }
        Command::New => {
            let boot = factory(super::SessionPick::New, active_endpoint, abort.clone())?;
            swap_agent(boot, state, input_tx, ui_tx);
        }
        Command::Resume(Some(choice)) => {
            if state.sessions_disabled {
                state.push_warning("/resume is disabled for --no-session runs");
                return Ok(false);
            }
            let Some(dir) = crate::agent::session::io::sessions_dir(cwd) else {
                anyhow::bail!("no HOME: nowhere to look for sessions");
            };
            let (sessions, _) = crate::agent::session::io::list_sessions(&dir);
            let chosen = match choice.parse::<usize>() {
                Ok(n) if n >= 1 => state
                    .pending_resume
                    .get(n - 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("{n} is not in the last /resume listing"))?,
                _ => crate::agent::session::io::resolve_prefix(&sessions, &choice)?,
            };
            let pick = super::SessionPick::Prefix(chosen.id.clone());
            let boot = factory(pick, active_endpoint, abort.clone())?;
            swap_agent(boot, state, input_tx, ui_tx);
        }
        Command::Resume(None) => {
            if state.sessions_disabled {
                state.push_warning("/resume is disabled for --no-session runs");
                return Ok(false);
            }
            let Some(dir) = crate::agent::session::io::sessions_dir(cwd) else {
                anyhow::bail!("no HOME: nowhere to look for sessions");
            };
            let (sessions, _) = crate::agent::session::io::list_sessions(&dir);
            if sessions.is_empty() {
                state.push_info("no sessions yet for this directory");
                return Ok(false);
            }
            state.pending_resume = sessions.clone();
            let mut listing = String::from("sessions (newest first):");
            for (i, session) in sessions.iter().enumerate() {
                listing.push_str(&format!(
                    "\n  {}) {} {} — {}",
                    i + 1,
                    session.timestamp,
                    session.id,
                    session.model,
                ));
            }
            listing.push_str("\n/resume <n|prefix> to open one");
            state.push_info(&listing);
        }
        Command::Model(None) => {
            let discovered =
                crate::llm::discovery::discover_local_providers(&config.providers).await;
            let rows =
                crate::config::providers::catalog_with_discovered(&config.providers, &discovered);
            state.pending_models = rows
                .iter()
                .map(|r| (r.provider.clone(), r.model.clone()))
                .collect();
            let mut listing = String::from("models:");
            for (i, row) in rows.iter().enumerate() {
                let active = if row.provider == active_endpoint.provider
                    && row.model == active_endpoint.model
                {
                    " [active]"
                } else {
                    ""
                };
                let proto = match row.api {
                    crate::llm::ApiKind::OpenAiCompletions => "",
                    crate::llm::ApiKind::AnthropicMessages => " · anthropic",
                };
                let status_note = row
                    .status
                    .as_deref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default();
                let key_note = if row.needs_key && !row.has_key {
                    " [no key]"
                } else {
                    ""
                };
                listing.push_str(&format!(
                    "\n  {}) {}/{} (ctx {}k{}{}{}){}",
                    i + 1,
                    row.provider,
                    row.model,
                    row.context_window / 1000,
                    status_note,
                    proto,
                    key_note,
                    active,
                ));
            }
            listing.push_str("\n/model <n|provider|provider/model> to switch");
            state.push_info(&listing);
        }
        Command::Model(Some(choice)) => {
            let target = match choice.parse::<usize>() {
                Ok(n) if n >= 1 => {
                    let (p, m) = state
                        .pending_models
                        .get(n - 1)
                        .ok_or_else(|| anyhow::anyhow!("{n} is not in the last /model listing"))?;
                    format!("{p}/{m}")
                }
                _ => choice,
            };
            let mut endpoint =
                config.resolve_switch_with_discovered(&target, &state.pending_models)?;
            endpoint.require_auth()?;
            crate::llm::client::maybe_probe_local_health(&mut endpoint).await;
            let _ = crate::llm::client::LlmClient::from_endpoint(&endpoint)?;
            input_tx
                .send(super::AgentInput::SwitchEndpoint(endpoint.clone()))
                .await?;
            *active_endpoint = endpoint.clone();
            state.set_model(&endpoint.label(), endpoint.context_window);
            state.push_info(&format!(
                "switched to {} ({}, ctx {}k)",
                endpoint.label(),
                endpoint.api,
                endpoint.context_window / 1000
            ));
        }
        Command::Unknown { name, args } => {
            // Not built-in — but it may be a prompt template (M7.4):
            // the body (plus args) lands in the editor for review, and
            // Enter submits it. Insertion, not submission: a template is
            // a draft the user edits, not a command that acts.
            if let Some(template) = prompts.iter().find(|t| t.name == name) {
                let draft = if args.is_empty() {
                    format!("{}\n", template.body)
                } else {
                    format!("{}\n{}\n", template.body, args)
                };
                state.editor.insert_paste(&draft);
            } else {
                state.push_warning(&format!(
                    "unknown command /{name} — valid: {} (or a prompt template, /prompts lists them)",
                    known_commands()
                ));
            }
        }
    }
    Ok(false)
}

/// Point the session at a new agent: fresh input channel, fresh agent
/// task (the old one exits when its channel closes — commands only run
/// while idle, so no run is in flight to orphan), seeded transcript.
fn swap_agent(
    boot: super::Boot,
    state: &mut super::view::State,
    input_tx: &mut tokio::sync::mpsc::Sender<super::AgentInput>,
    ui_tx: &tokio::sync::mpsc::Sender<super::agent_task::UiEvent>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<super::AgentInput>(16);
    *input_tx = tx;
    state.new_session(boot.session_path, boot.sessions_disabled);
    state.seed_from_entries(boot.agent.entries());
    tokio::spawn(super::agent_task::agent_task(boot.agent, rx, ui_tx.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_goes_to_the_model() {
        assert_eq!(
            classify_submission("  fix the test  "),
            Submission::Plain("fix the test".into())
        );
        assert_eq!(classify_submission(""), Submission::Plain("".into()));
    }

    #[test]
    fn slash_lines_classify_to_commands() {
        assert_eq!(
            classify_submission("/new"),
            Submission::Command(Command::New)
        );
        assert_eq!(
            classify_submission("/quit"),
            Submission::Command(Command::Quit)
        );
        assert_eq!(
            classify_submission("/exit"),
            Submission::Command(Command::Quit)
        );
        assert_eq!(
            classify_submission("/hooks"),
            Submission::Command(Command::Hooks)
        );
        assert_eq!(
            classify_submission("/skills"),
            Submission::Command(Command::Skills)
        );
        assert_eq!(
            classify_submission("/session"),
            Submission::Command(Command::Session)
        );
    }

    #[test]
    fn resume_argument_shapes() {
        assert_eq!(
            classify_submission("/resume"),
            Submission::Command(Command::Resume(None))
        );
        assert_eq!(
            classify_submission("/resume   "),
            Submission::Command(Command::Resume(None))
        );
        assert_eq!(
            classify_submission("/resume 2"),
            Submission::Command(Command::Resume(Some("2".into())))
        );
        assert_eq!(
            classify_submission("/resume ab12cd"),
            Submission::Command(Command::Resume(Some("ab12cd".into())))
        );
    }

    #[test]
    fn skill_forcing_takes_name_and_args() {
        assert_eq!(
            classify_submission("/skill:pdf-tools"),
            Submission::Command(Command::Skill {
                name: "pdf-tools".into(),
                args: String::new(),
            })
        );
        assert_eq!(
            classify_submission("/skill:pdf-tools  report.pdf --tables"),
            Submission::Command(Command::Skill {
                name: "pdf-tools".into(),
                args: "report.pdf --tables".into(),
            })
        );
        assert_eq!(
            classify_submission("/skill:"),
            Submission::Command(Command::Skill {
                name: String::new(),
                args: String::new(),
            })
        );
    }

    #[test]
    fn compact_takes_optional_focus_instructions() {
        assert_eq!(
            classify_submission("/compact"),
            Submission::Command(Command::Compact(None))
        );
        assert_eq!(
            classify_submission("/compact   "),
            Submission::Command(Command::Compact(None))
        );
        assert_eq!(
            classify_submission("/compact focus on the parsing rework"),
            Submission::Command(Command::Compact(Some("focus on the parsing rework".into())))
        );
        assert!(known_commands().contains("/compact"));
    }

    #[test]
    fn unknown_commands_say_so_with_the_valid_set() {
        assert_eq!(
            classify_submission("/definitely-not-a-command"),
            Submission::Command(Command::Unknown {
                name: "definitely-not-a-command".into(),
                args: String::new(),
            })
        );
        assert_eq!(
            classify_submission("/frobnicate  the args"),
            Submission::Command(Command::Unknown {
                name: "frobnicate".into(),
                args: "the args".into(),
            })
        );
        assert!(known_commands().contains("/new"));
        assert!(known_commands().contains("/prompts"));
    }

    #[test]
    fn bang_prefixes_run_shell_with_or_without_context() {
        assert_eq!(
            classify_submission("!echo hello"),
            Submission::Shell {
                command: "echo hello".into(),
                to_context: true,
            }
        );
        assert_eq!(
            classify_submission("!!cargo test -p x"),
            Submission::Shell {
                command: "cargo test -p x".into(),
                to_context: false,
            }
        );
        // A bare `!` is not a shell line — falls through to the model.
        assert_eq!(classify_submission("!"), Submission::Plain("!".into()));
    }

    /// `/name` for a loaded template lands in the EDITOR as a draft
    /// (args appended), never submitted; an unknown name keeps the
    /// warning. The factory is never called on this path, so it can be
    /// a stub.
    #[tokio::test]
    async fn unknown_name_matching_a_template_inserts_a_draft() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let mut state = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(vec![]),
            std::sync::Arc::new(vec![]),
        );
        let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
        let factory: super::super::Factory =
            Arc::new(|_, _, _| unreachable!("no /new on this path"));
        let config = crate::config::Config::default();
        let mut active_endpoint = config.active_endpoint();
        let prompts = vec![crate::prompts::PromptTemplate {
            name: "review".into(),
            description: "Review a diff".into(),
            path: "/home/x/.gg/prompts/review.md".into(),
            body: "Review this diff line by line.".into(),
        }];
        let skills: Vec<crate::agent::skills::Skill> = Vec::new();
        let hooks = crate::agent::hooks::Hooks::default();
        let abort = Arc::new(AtomicBool::new(false));
        let cwd = std::path::PathBuf::from("/tmp/x");

        let ctx = CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: &skills,
            prompts: &prompts,
            hooks: &hooks,
            cwd: &cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        };
        handle_command(
            Command::Unknown {
                name: "review".into(),
                args: "src/main.rs".into(),
            },
            ctx,
        )
        .await
        .unwrap();
        let drafted: Vec<&str> = state.editor.lines();
        assert_eq!(
            drafted,
            vec!["Review this diff line by line.", "src/main.rs", ""]
        );
        // Nothing was sent to the agent — the draft awaits Enter.
        assert!(input_rx.try_recv().is_err());

        // A genuinely unknown name warns and leaves the editor alone.
        let mut state2 = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(vec![]),
            std::sync::Arc::new(vec![]),
        );
        let (mut input_tx2, _rx2) = tokio::sync::mpsc::channel(4);
        let ctx = CmdCtx {
            state: &mut state2,
            factory: &factory,
            input_tx: &mut input_tx2,
            ui_tx: &ui_tx,
            skills: &skills,
            prompts: &prompts,
            hooks: &hooks,
            cwd: &cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        };
        handle_command(
            Command::Unknown {
                name: "nope".into(),
                args: String::new(),
            },
            ctx,
        )
        .await
        .unwrap();
        assert!(state2.editor.lines() == vec![""]);
    }

    #[tokio::test]
    async fn model_command_lists_and_switches_endpoints() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let mut state = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            "strata/qwen3.8-flash-next-iq3_s",
            131_072,
            Arc::new(vec![]),
            Arc::new(vec![]),
        );
        let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
        let factory: super::super::Factory =
            Arc::new(|_, _, _| unreachable!("no /new on this path"));
        let mut config = crate::config::Config::default();
        config.providers.get_mut("deepseek").unwrap().api_key = Some("test-key".into());
        let mut active_endpoint = config.active_endpoint();
        let skills: Vec<crate::agent::skills::Skill> = Vec::new();
        let prompts: Vec<crate::prompts::PromptTemplate> = Vec::new();
        let abort = Arc::new(AtomicBool::new(false));
        let cwd = std::path::PathBuf::from("/tmp/x");

        handle_command(
            Command::Model(None),
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &skills,
                prompts: &prompts,
                hooks: &crate::agent::hooks::Hooks::default(),
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();
        assert!(!state.pending_models.is_empty());

        handle_command(
            Command::Model(Some("deepseek/deepseek-flash".into())),
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &skills,
                prompts: &prompts,
                hooks: &crate::agent::hooks::Hooks::default(),
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();
        assert_eq!(active_endpoint.label(), "deepseek/deepseek-flash");
        assert!(matches!(
            input_rx.try_recv().unwrap(),
            super::super::AgentInput::SwitchEndpoint(_)
        ));
    }

    /// `/hooks` surfaces the loaded set and `--no-hooks` without reaching
    /// the agent.
    #[tokio::test]
    async fn hooks_lists_loaded_or_disabled() {
        use crate::agent::hooks::{Hook, HookEvent, Hooks, load_from};
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let mut state = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            Arc::new(vec![]),
            Arc::new(vec![]),
        );
        let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
        let factory: super::super::Factory =
            Arc::new(|_, _, _| unreachable!("no /new on this path"));
        let abort = Arc::new(AtomicBool::new(false));
        let cwd = std::path::PathBuf::from("/tmp/x");
        let config = crate::config::Config::default();
        let mut active_endpoint = config.active_endpoint();
        let hooks = Hooks::new(vec![Hook {
            event: HookEvent::PreTool,
            tools: Some(vec!["bash".into()]),
            command: "guard.sh".into(),
            timeout_secs: 5,
        }]);

        handle_command(
            Command::Hooks,
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &[],
                prompts: &[],
                hooks: &hooks,
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();
        let lines = state.line_texts_for_test();
        assert!(lines.iter().any(|l| l == "hooks (1):"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("pre_tool") && l.contains("guard.sh")),
            "{lines:?}"
        );

        let (disabled, _) = load_from(None, &cwd, true).unwrap();
        handle_command(
            Command::Hooks,
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &[],
                prompts: &[],
                hooks: &disabled,
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();
        let lines = state.line_texts_for_test();
        assert!(lines.iter().any(|l| l.contains("disabled by --no-hooks")));
    }

    #[tokio::test]
    async fn model_command_discovers_local_router_and_strata_health() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /health ") {
                    r#"{"status":"ok","max_context":32768,"model":"inkling-2-mini-sq-15","service":"strata"}"#
                } else {
                    r#"{"data":[{"id":"inkling-2-mini-sq-15","status":{"value":"loaded"},"max_model_len":32768},{"id":"gemma4-26b-200k","status":{"value":"unloaded","args":["llama-server","--ctx-size","200000"]}}]}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                s.write_all(resp.as_bytes()).unwrap();
            }
        });

        let toml = format!(
            "[providers.strata]\nbase_url = \"http://{addr}/v1\"\n[providers.llama-cpp]\nbase_url = \"http://{addr}/v1\"\nmodel = \"gemma4-26b-200k\"\n"
        );
        let config =
            crate::config::merge(crate::config::Config::default(), Some(&toml), None, &[]).unwrap();
        let mut active_endpoint = config.active_endpoint();
        let mut state = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            &active_endpoint.label(),
            active_endpoint.context_window,
            Arc::new(vec![]),
            Arc::new(vec![]),
        );
        let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
        let factory: super::super::Factory =
            Arc::new(|_, _, _| unreachable!("no /new on this path"));
        let abort = Arc::new(AtomicBool::new(false));
        let cwd = std::path::PathBuf::from("/tmp/x");

        handle_command(
            Command::Model(None),
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &[],
                prompts: &[],
                hooks: &crate::agent::hooks::Hooks::default(),
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();

        let lines = state.line_texts_for_test();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("llama-cpp/gemma4-26b-200k (ctx 200k · unloaded)")),
            "{lines:?}"
        );

        handle_command(
            Command::Model(Some("strata".into())),
            CmdCtx {
                state: &mut state,
                factory: &factory,
                input_tx: &mut input_tx,
                ui_tx: &ui_tx,
                skills: &[],
                prompts: &[],
                hooks: &crate::agent::hooks::Hooks::default(),
                cwd: &cwd,
                abort: &abort,
                config: &config,
                active_endpoint: &mut active_endpoint,
            },
        )
        .await
        .unwrap();
        assert_eq!(active_endpoint.label(), "strata/inkling-2-mini-sq-15");
        assert_eq!(active_endpoint.context_window, 32_768);
    }
}
