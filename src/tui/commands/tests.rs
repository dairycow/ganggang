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

/// `/skills` is gone: submitting it warns that `/skill` lists skills.
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
            .any(|l| l.contains("/skills was removed: type /skill to list skills")),
        "{lines:?}"
    );
}

/// The submit path's own `/session` answer is the list's `info` block:
/// one implementation, so the two cannot say different things.
#[tokio::test]
async fn the_session_command_prints_the_same_block_as_the_info_row() {
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
    let label = active_endpoint.label();
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    handle_command(
        Command::Session,
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
    assert_eq!(
        state.line_texts_for_test(),
        [
            "session: not persisted (--no-session)".to_string(),
            format!("model: {label}"),
            "entries: 0".to_string(),
            "tokens: 0 in / 0 out / 0 cache".to_string(),
        ]
    );
}

/// A bare `/model` that reaches the submit path points at the list: the
/// list has the composer while the word is typed, so this is its answer
/// for a submit that got past it.
#[tokio::test]
async fn the_bare_model_command_points_at_the_list() {
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
    assert!(
        state
            .line_texts_for_test()
            .iter()
            .any(|line| line.contains("pick a model from the list above the composer")),
        "{:?}",
        state.line_texts_for_test()
    );
}

#[test]
fn resume_is_not_a_command_and_session_takes_any_argument() {
    assert_eq!(
        classify_submission("/resume"),
        Submission::Command(Command::Unknown("resume".into()))
    );
    assert_eq!(
        classify_submission("/resume   "),
        Submission::Command(Command::Unknown("resume".into()))
    );
    assert_eq!(
        classify_submission("/session"),
        Submission::Command(Command::Session)
    );
    // The action rows' words and a filter: the argument is ignored, the
    // command is the bare one.
    assert_eq!(
        classify_submission("/session copy"),
        Submission::Command(Command::Session)
    );
    assert_eq!(
        classify_submission("/session 3fa9"),
        Submission::Command(Command::Session)
    );
}

/// The catalogue: no `/resume` row, the `/session` row's argument words,
/// and the unknown-command line's valid set says so.
#[test]
fn the_catalogue_names_session_not_resume() {
    let builtins: Vec<String> = super::super::suggest::BUILTINS
        .iter()
        .map(|(name, args, description)| format!("{name}{args} {description}"))
        .collect();
    assert!(
        builtins.iter().all(|row| !row.starts_with("/resume")),
        "no /resume row: {builtins:?}"
    );
    assert!(
        builtins
            .iter()
            .any(|row| row == "/session [filter|copy|info] pick a saved session, copy its path, or show session info"),
        "the /session row: {builtins:?}"
    );
    assert!(known_commands().contains("/session [filter|copy|info]"));
    assert!(!known_commands().contains("/resume"));
}

#[test]
fn skill_forcing_is_gone_and_the_space_form_classifies() {
    // The old colon form is not a command: it falls to the
    // unknown-command reply, where the valid set names the space form.
    assert_eq!(
        classify_submission("/skill:pdf-tools"),
        Submission::Command(Command::Unknown("skill:pdf-tools".into()))
    );
    assert!(!known_commands().contains("/skill:"));
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

/// Drive one `/skill <name>` command end to end: a temp cwd holding
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

/// `/skill <name>` with an `@file` in the args: the file's content
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
async fn model_command_switches_endpoints() {
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
    // The session is on `strata` (the label above), whichever provider is
    // the shipped default.
    config.apply_cli(Some("strata"), None).unwrap();
    config.providers.get_mut("deepseek").unwrap().api_key = Some("test-key".into());
    config.providers.get_mut("openrouter").unwrap().api_key = Some("test-key".into());
    let mut active_endpoint = config.active_endpoint();
    let skills = Arc::new(Vec::new());
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");
    macro_rules! model {
        ($choice:expr) => {
            handle_command(
                Command::Model($choice),
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
            .unwrap()
        };
    }

    // No argument: the models are the list's to show, so the handler has
    // one line to say, and switches nothing.
    model!(None);
    let lines = state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("pick a model from the list above the composer")),
        "{lines:?}"
    );
    assert!(input_rx.try_recv().is_err(), "nothing was switched");

    model!(Some("deepseek/deepseek-flash".into()));
    assert_eq!(active_endpoint.label(), "deepseek/deepseek-flash");
    assert!(matches!(
        input_rx.try_recv().unwrap(),
        super::super::AgentInput::SwitchEndpoint(_)
    ));

    // The model in use again: said, and nothing is switched or probed.
    model!(Some("deepseek/deepseek-flash".into()));
    let lines = state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("already on deepseek/deepseek-flash")),
        "{lines:?}"
    );
    assert!(input_rx.try_recv().is_err(), "no second switch");

    // An OpenRouter id carries a slash of its own: the command the list
    // builds for it has two, and the first still names the provider.
    model!(Some("openrouter/upstage/solar-mini-4-5".into()));
    assert_eq!(active_endpoint.provider, "openrouter");
    assert_eq!(active_endpoint.model, "upstage/solar-mini-4-5");
    assert!(matches!(
        input_rx.try_recv().unwrap(),
        super::super::AgentInput::SwitchEndpoint(_)
    ));
}

#[tokio::test]
async fn model_command_switches_to_a_discovered_model_and_probes_health() {
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
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::channel(4);
    let factory: super::super::Factory =
        Arc::new(|_, _, _, _| unreachable!("no /new on this path"));
    let abort = Arc::new(AtomicBool::new(false));
    let mut cwd = std::path::PathBuf::from("/tmp/x");

    // The background discovery the loop starts: its rows reach the state
    // as the event the loop applies, and are the list's rows.
    super::super::models::spawn_model_discovery(config.providers.clone(), ui_tx.clone());
    let event = ui_rx.recv().await.unwrap();
    assert!(
        super::super::agent_task::apply(event, &mut state, &mut input_tx).await,
        "the event folds into state without ending the TUI"
    );
    state.editor.set_text("/model");
    state.sync_picker();
    let rows = state.picker_matches();
    let discovered = rows.iter().find_map(|row| match row {
        super::super::picker::Row::Model { suggestion, .. }
            if suggestion.label == "llama-cpp/gemma4-26b-200k" =>
        {
            Some(suggestion.description.clone())
        }
        _ => None,
    });
    assert_eq!(
        discovered.as_deref(),
        Some("ctx 200k · unloaded"),
        "the list shows the discovered model with its status: {rows:?}"
    );

    // A model the config does not list: known to the switch because the
    // list's rows have it.
    handle_command(
        Command::Model(Some("llama-cpp/gemma4-26b-200k".into())),
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
    assert_eq!(active_endpoint.label(), "llama-cpp/gemma4-26b-200k");

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

#[test]
fn skill_takes_name_and_args() {
    // The name is the first word of the argument, the args the rest:
    // `/skill self-improve --last 10` runs self-improve with `--last 10`.
    assert_eq!(
        classify_submission("/skill self-improve --last 10"),
        Submission::Command(Command::Skill {
            name: "self-improve".into(),
            args: "--last 10".into(),
        })
    );
    // A bare `/skill` is an empty name and empty args: the list opens
    // instead, so the handler never sees it from the TUI; a bare name
    // has no args.
    assert_eq!(
        classify_submission("/skill"),
        Submission::Command(Command::Skill {
            name: String::new(),
            args: String::new(),
        })
    );
    assert_eq!(
        classify_submission("/skill foo"),
        Submission::Command(Command::Skill {
            name: "foo".into(),
            args: String::new(),
        })
    );
    // The colon form is not a command: it falls to the unknown-command
    // reply, where the valid set now names `/skill <name> [args]`.
    assert_eq!(
        classify_submission("/skill:foo"),
        Submission::Command(Command::Unknown("skill:foo".into()))
    );
    assert!(!known_commands().contains("/skill:"));
    assert!(known_commands().contains("/skill <name> [args]"));
}

/// `suggest` no longer lists skill rows: `/skill` is the trigger's own
/// word, and the built-in row's completion is the space form.
mod suggest_skill_tests {
    use crate::tui::suggest;

    #[test]
    fn the_bare_skill_word_has_no_suggestion_rows() {
        assert_eq!(
            suggest::suggestions("/skill"),
            Vec::new(),
            "the trigger's word: the list's own, not the popup's"
        );
    }

    #[test]
    fn a_partial_skill_word_completes_to_the_space_form() {
        // A partial word: only `/skil` is unambiguous — `/skill` itself
        // is the trigger's own word, never a popup row.
        let got = suggest::suggestions("/skil");
        assert_eq!(got.len(), 1, "the generic row: {got:?}");
        assert_eq!(got[0].label, "/skill <name> [args]");
        assert_eq!(got[0].completion, "/skill ");
    }
}
