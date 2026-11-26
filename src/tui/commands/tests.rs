use super::super::history::History;
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
        Submission::Command(Command::Unknown("skills".into()))
    );
    assert_eq!(
        classify_submission("/session"),
        Submission::Command(Command::Session)
    );
}

#[test]
fn reload_classifies_and_is_known() {
    assert_eq!(
        classify_submission("/reload"),
        Submission::Command(Command::Reload)
    );
    assert!(known_commands().contains("/reload"));
}

/// `/reload` forwards to the agent task, which re-discovers and answers
/// with `UiEvent::Reloaded` — no run begins.
#[tokio::test]
async fn reload_forwards_to_the_agent_task() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let mut state = super::super::view::State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        Arc::new(vec![]),
        History::default(),
    );
    let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let config = crate::config::Config::default();
    let mut active_endpoint = config.active_endpoint();
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Reload,
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: Arc::new(Vec::new()),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        input_rx.try_recv().unwrap(),
        super::super::AgentInput::Reload
    ));
}

/// `/skills` is gone: submitting it warns that `/skill:` lists skills.
#[tokio::test]
async fn skills_command_is_removed_with_a_hint() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let mut state = super::super::view::State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        Arc::new(vec![]),
        History::default(),
    );
    let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let config = crate::config::Config::default();
    let mut active_endpoint = config.active_endpoint();
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Unknown("skills".into()),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: Arc::new(Vec::new()),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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
            .any(|l| l.contains("/skills was removed: type /skill: to list skills")),
        "{lines:?}"
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
        Submission::Command(Command::Unknown("definitely-not-a-command".into()))
    );
    assert_eq!(
        classify_submission("/frobnicate  the args"),
        Submission::Command(Command::Unknown("frobnicate".into()))
    );
    assert!(known_commands().contains("/new"));
    assert!(!known_commands().contains("/prompts"));
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

/// Drive one `/skill:` command end to end: a temp cwd holding
/// `notes.txt`, one skill with the given instructions, the command
/// with the given args, and the resulting state plus what reached
/// the agent (if anything). The factory is never called on this
/// path, so it can be a stub.
async fn run_skill_command(
    tag: &str,
    args: &str,
    instructions: &str,
) -> (super::super::view::State, Option<super::super::AgentInput>) {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let mut dir = std::env::temp_dir().join(format!("gg-skill-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "attach me\n").unwrap();

    let mut state =
        super::super::view::State::new(&dir, "m", 131_072, Arc::new(vec![]), History::default());
    let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let config = crate::config::Config::default();
    let mut active_endpoint = config.active_endpoint();
    let skills = Arc::new(vec![crate::agent::skills::Skill {
        name: "pdf-tools".into(),
        description: "d".into(),
        disable_model_invocation: false,
        path: dir.join("SKILL.md"),
        instructions: instructions.into(),
    }]);
    let abort = Arc::new(AtomicBool::new(false));

    handle_command(
        Command::Skill {
            name: "pdf-tools".into(),
            args: args.into(),
        },
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut dir,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (state, input_rx.try_recv().ok())
}

/// `/skill:<name>` with an `@file` in the args: the file's content
/// rides into the Prompt payload, while the transcript line stays
/// exactly as typed — no attachment blocks on screen.
#[tokio::test]
async fn skill_args_mentions_attach_to_the_prompt_not_the_transcript() {
    let (state, sent) = run_skill_command("args", "summarise @notes.txt", "Read the pdf.").await;
    match sent {
        Some(super::super::AgentInput::Prompt(payload)) => {
            assert!(payload.contains("Follow this skill's instructions for: summarise @notes.txt"));
            assert!(payload.contains("--- @notes.txt (1 lines) ---\nattach me\n"));
            assert!(payload.contains("--- end @notes.txt ---"));
            let body_at = payload.find("Read the pdf.").unwrap();
            let attach_at = payload.find("--- attachments ---").unwrap();
            assert!(body_at < attach_at, "attachments follow skill instructions");
        }
        other => panic!("expected a Prompt, got {other:?}"),
    }
    let lines = state.line_texts_for_test();
    assert!(lines.iter().any(|l| l.contains("summarise @notes.txt")));
    assert!(!lines.iter().any(|l| l.contains("attach me")));
    assert!(!lines.iter().any(|l| l.contains("attachments")));
}

/// An `@path` inside a skill's own instructions is the skill's text,
/// not a user mention: nothing is attached even though `notes.txt`
/// sits in the cwd.
#[tokio::test]
async fn skill_instructions_mentions_are_not_expanded() {
    let (_state, sent) = run_skill_command("body", "", "See @notes.txt for detail.").await;
    match sent {
        Some(super::super::AgentInput::Prompt(payload)) => {
            assert!(payload.contains("See @notes.txt for detail."));
            assert!(!payload.contains("attachments"));
            assert!(!payload.contains("attach me"));
        }
        other => panic!("expected a Prompt, got {other:?}"),
    }
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
        History::default(),
    );
    let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let mut config = crate::config::Config::default();
    config.providers.get_mut("deepseek").unwrap().api_key = Some("test-key".into());
    let mut active_endpoint = config.active_endpoint();
    let skills = Arc::new(Vec::new());
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Model(None),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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
        History::default(),
    );
    let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");
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
            skills: Arc::new(Vec::new()),
            hooks: &hooks,
            cwd: &mut cwd,
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
            skills: Arc::new(Vec::new()),
            hooks: &disabled,
            cwd: &mut cwd,
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
        History::default(),
    );
    let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Model(None),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: Arc::new(Vec::new()),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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
            skills: Arc::new(Vec::new()),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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

#[test]
fn think_argument_shapes() {
    assert_eq!(
        classify_submission("/think"),
        Submission::Command(Command::Think(None))
    );
    assert_eq!(
        classify_submission("/think   "),
        Submission::Command(Command::Think(None))
    );
    assert_eq!(
        classify_submission("/think xhigh"),
        Submission::Command(Command::Think(Some("xhigh".into())))
    );
    assert_eq!(
        classify_submission("/think low 2048"),
        Submission::Command(Command::Think(Some("low 2048".into())))
    );
    assert_eq!(
        classify_submission("/think off"),
        Submission::Command(Command::Think(Some("off".into())))
    );
    assert_eq!(
        classify_submission("/think default"),
        Submission::Command(Command::Think(Some("default".into())))
    );
    assert!(known_commands().contains("/think [effort|off|default] [budget]"));
}

#[test]
fn parse_think_args_covers_every_valid_form() {
    // `off`/`none` sets effort to `"none"` and clears the budget.
    assert_eq!(
        parse_think_args("off", Some("high"), Some(4096)).unwrap(),
        (Some("none".into()), None)
    );
    assert_eq!(
        parse_think_args("NONE", Some("high"), Some(4096)).unwrap(),
        (Some("none".into()), None)
    );
    // `default`/`clear` resets both to provider default.
    assert_eq!(
        parse_think_args("default", Some("high"), Some(4096)).unwrap(),
        (None, None)
    );
    assert_eq!(
        parse_think_args("CLEAR", Some("high"), Some(4096)).unwrap(),
        (None, None)
    );
    // A bare number touches only the budget; 0 clears it.
    assert_eq!(
        parse_think_args("2048", Some("high"), Some(4096)).unwrap(),
        (Some("high".into()), Some(2048))
    );
    assert_eq!(
        parse_think_args("0", Some("high"), Some(4096)).unwrap(),
        (Some("high".into()), None)
    );
    // A bare word touches only the effort.
    assert_eq!(
        parse_think_args("xhigh", Some("low"), Some(4096)).unwrap(),
        (Some("xhigh".into()), Some(4096))
    );
    // `effort budget` sets both; a 0 budget clears it.
    assert_eq!(
        parse_think_args("low 2048", None, None).unwrap(),
        (Some("low".into()), Some(2048))
    );
    assert_eq!(
        parse_think_args("high 0", None, None).unwrap(),
        (Some("high".into()), None)
    );
}

#[test]
fn parse_think_args_rejects_malformed_input() {
    for raw in [
        "",
        "off 2048",
        "none 2048",
        "default 2048",
        "clear 2048",
        "5 2048",
        "low abc",
        "a b c",
    ] {
        let err = parse_think_args(raw, Some("high"), Some(4096)).unwrap_err();
        assert!(
            err.to_string()
                .contains("usage: /think [effort|off|default] [budget]"),
            "{raw:?}: {err}"
        );
    }
}

/// `/think` against a stubbed active endpoint: bare shows the current
/// configuration, an argument updates it and forwards the endpoint,
/// malformed input errors without forwarding.
#[tokio::test]
async fn think_command_shows_and_updates_the_setting() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let mut state = super::super::view::State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        Arc::new(vec![]),
        History::default(),
    );
    let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
    let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let config = crate::config::Config::default();
    let mut active_endpoint = config.active_endpoint();
    active_endpoint.reasoning_effort = Some("medium".into());
    active_endpoint.reasoning_budget_tokens = Some(4096);
    let skills = Arc::new(Vec::new());
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Think(None),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
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
            .any(|l| l == "thinking: effort medium, budget 4096 tokens"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "/think [effort|off|default] [budget] to change"),
        "{lines:?}"
    );

    handle_command(
        Command::Think(Some("xhigh".into())),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert_eq!(active_endpoint.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(active_endpoint.reasoning_budget_tokens, Some(4096));
    match input_rx.try_recv().unwrap() {
        super::super::AgentInput::SwitchEndpoint(endpoint) => {
            assert_eq!(endpoint.reasoning_effort.as_deref(), Some("xhigh"));
            assert_eq!(endpoint.reasoning_budget_tokens, Some(4096));
        }
        other => panic!("expected SwitchEndpoint, got {other:?}"),
    }
    let lines = state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l == "thinking: effort xhigh, budget 4096 tokens"),
        "{lines:?}"
    );

    handle_command(
        Command::Think(Some("off".into())),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert_eq!(active_endpoint.reasoning_effort.as_deref(), Some("none"));
    assert!(active_endpoint.reasoning_budget_tokens.is_none());
    let lines = state.line_texts_for_test();
    assert!(
        lines.iter().any(|l| l == "thinking: effort none"),
        "{lines:?}"
    );
    assert!(matches!(
        input_rx.try_recv().unwrap(),
        super::super::AgentInput::SwitchEndpoint(endpoint)
        if endpoint.reasoning_effort.as_deref() == Some("none")
            && endpoint.reasoning_budget_tokens.is_none()
    ));

    handle_command(
        Command::Think(Some("default".into())),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert!(active_endpoint.reasoning_effort.is_none());
    assert!(active_endpoint.reasoning_budget_tokens.is_none());
    let lines = state.line_texts_for_test();
    assert!(
        lines.iter().any(|l| l == "thinking: provider default"),
        "{lines:?}"
    );
    assert!(matches!(
        input_rx.try_recv().unwrap(),
        super::super::AgentInput::SwitchEndpoint(endpoint)
        if endpoint.reasoning_effort.is_none()
            && endpoint.reasoning_budget_tokens.is_none()
    ));

    let err = handle_command(
        Command::Think(Some("low abc".into())),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("usage: /think"));
    assert!(input_rx.try_recv().is_err(), "nothing forwarded on error");

    // Anthropic endpoints do not support /think and must not mutate or forward.
    active_endpoint.api = crate::llm::ApiKind::AnthropicMessages;
    active_endpoint.reasoning_effort = Some("high".into());
    handle_command(
        Command::Think(None),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert!(
        state
            .line_texts_for_test()
            .iter()
            .any(|l| l == "thinking: not supported on anthropic-messages endpoints")
    );
    handle_command(
        Command::Think(Some("low".into())),
        CmdCtx {
            state: &mut state,
            factory: &factory,
            input_tx: &mut input_tx,
            ui_tx: &ui_tx,
            skills: skills.clone(),
            hooks: &crate::agent::hooks::Hooks::default(),
            cwd: &mut cwd,
            abort: &abort,
            config: &config,
            active_endpoint: &mut active_endpoint,
        },
    )
    .await
    .unwrap();
    assert_eq!(active_endpoint.reasoning_effort.as_deref(), Some("high"));
    assert!(
        input_rx.try_recv().is_err(),
        "nothing forwarded for anthropic"
    );
}
