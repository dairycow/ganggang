//! CLI entry point: mode dispatch (§15) — TTY ⇒ TUI, `-p` ⇒ print,
//! `--mode json` ⇒ events. All share one agent core and persist a
//! session by default.

mod agent;
mod config;
#[cfg(test)]
mod doc_sync;
mod events;
mod export;
mod home;
mod json_mode;
mod llm;
mod nested;
mod printers;
mod tool_args;
mod tui;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::io::IsTerminal as _;
use std::io::Write as _;

use anyhow::{Context, bail};
use clap::Parser;

use crate::agent::Agent;
use crate::agent::tools::default_registry;
use crate::llm::client::LlmClient;
use crate::printers::Printer;

#[derive(Parser)]
#[command(name = "gg", version, about = "A small Rust agentic harness")]
struct Cli {
    /// One-shot print mode: run this prompt and print the final answer.
    /// A prompt of "-" reads the prompt text from stdin.
    #[arg(short, long = "print")]
    prompt: Option<String>,

    /// Comma-separated tool allowlist, e.g. "read,bash". Overrides the
    /// `tools` setting from config files. A nested run can only name tools
    /// its parent run has.
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Continue the newest session for this directory.
    #[arg(short = 'c', long = "continue", conflicts_with_all = ["resume", "no_session"])]
    r#continue: bool,

    /// Resume a session by id (or a unique id prefix).
    #[arg(long, conflicts_with = "no_session")]
    resume: Option<String>,

    /// Do not persist a session for this run.
    #[arg(long)]
    no_session: bool,

    /// Output mode for one-shot runs: `print` (default: final
    /// text to stdout) or `json` (session header, then one AgentEvent
    /// per line).
    #[arg(long = "mode", value_enum, default_value_t = Mode::Print)]
    mode: Mode,

    /// Positional prompt, the `--mode json` spelling: gg --mode json "…".
    #[arg(value_name = "PROMPT")]
    prompt_positional: Option<String>,

    /// Export a session to a single self-contained HTML file and exit.
    /// SESSION is a file path or the id/8-hex prefix `--resume` takes;
    /// OUT_HTML defaults to `<session-stem>.html` in the cwd.
    #[arg(
        long = "export",
        num_args = 1..=2,
        value_name = "SESSION [OUT_HTML]",
        conflicts_with_all = ["prompt", "prompt_positional"]
    )]
    export: Option<Vec<String>>,

    /// Development mode: watch the ganggang sources, rebuild on change,
    /// and re-exec the new binary on the same session.
    #[arg(long, conflicts_with_all = ["prompt", "prompt_positional", "export"])]
    watch: bool,

    /// Provider preset or configured provider name (e.g. "strata",
    /// "glm-coding-plan", "opencode-go", "deepseek", "openrouter").
    #[arg(short = 'P', long)]
    provider: Option<String>,

    /// Model id or `provider/model` (overrides config and provider default).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// List configured providers and models (probing local servers), then exit.
    #[arg(
        long = "list-models",
        conflicts_with_all = ["prompt", "prompt_positional", "export", "watch"]
    )]
    list_models: bool,

    /// Skip every hook for this run, user and project, including any warning about a project hooks file. Ignored in a nested run, which keeps its parent's hooks.
    #[arg(long)]
    no_hooks: bool,

    /// Reasoning effort sent to supporting endpoints (e.g. "low", "medium", "high", "xhigh", "none").
    /// Pass "" to clear a provider preset's default.
    #[arg(long = "reasoning-effort", value_name = "EFFORT")]
    reasoning_effort: Option<String>,

    /// Cap thinking/reasoning tokens per turn (Strata/local backends). 0 disables any preset cap.
    #[arg(long = "thinking-budget", value_name = "TOKENS")]
    thinking_budget: Option<u32>,

    /// Maximum turns per run (0 disables). Soft wrap-up nudge at 80% of budget.
    /// A nested run cannot exceed its parent's.
    #[arg(long = "max-turns", value_name = "N")]
    max_turns: Option<u32>,

    /// Maximum wall-clock seconds per run (0 disables). Soft wrap-up nudge at 80% of budget.
    /// A nested run cannot exceed its parent's.
    #[arg(long = "max-seconds", value_name = "SECS")]
    max_seconds: Option<u64>,

    /// Persist and/or resume this exact session JSONL file path.
    #[arg(
        long = "session-file",
        value_name = "PATH",
        conflicts_with_all = ["continue", "resume", "no_session"]
    )]
    session_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Print,
    Json,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let abort = Arc::new(AtomicBool::new(false));

    if cli.watch {
        if cli.mode == Mode::Json {
            bail!("--watch is a TUI mode; it cannot combine with --mode json");
        }
        if cli.no_session {
            bail!("--watch needs a session to continue after a restart; drop --no-session");
        }
    }

    // Export is its own mode: file in, HTML out, no agent, no server.
    if let Some(args) = &cli.export {
        let cwd = std::env::current_dir().context("determining cwd")?;
        let session = export::resolve_target(&args[0], &cwd)?;
        let out = match args.get(1) {
            Some(out) => std::path::PathBuf::from(out),
            None => export::default_out_path(&session, &cwd),
        };
        export::export_file(&session, &out)?;
        println!("exported {} → {}", session.display(), out.display());
        return Ok(());
    }

    if cli.list_models {
        let mut cfg = config::load()?;
        cfg.apply_cli(cli.provider.as_deref(), cli.model.as_deref())?;
        cfg.require_provider_for_base_url()?;
        let mut active = cfg.active_endpoint();
        llm::health::maybe_probe_local_health(&mut active).await;
        report_fallback(&cfg, &active);
        if let Some(p) = cfg.providers.get_mut(&active.provider)
            && !p.context_window_explicit
        {
            p.context_window = active.context_window;
        }
        let discovered = llm::discovery::discover_local_providers(&cfg.providers).await;
        for row in config::providers::catalog_with_discovered(&cfg.providers, &discovered) {
            let mark = if row.provider == active.provider && row.model == active.model {
                "*"
            } else {
                " "
            };
            // The table labels keys from the provider table, which never
            // sees `GG_API_KEY` or a flat `api_key` key, and knows nothing
            // of a blank `GG_API_KEY` clearing one: a row of the active
            // provider shows the key the run would use. The table itself is
            // left alone, so discovery does not carry that key to the
            // provider's own address.
            let has_key = if row.provider == active.provider {
                active.api_key.is_some()
            } else {
                row.has_key
            };
            let key_status = if !row.needs_key {
                "local"
            } else if has_key {
                "ok"
            } else {
                "no key"
            };
            let status_note = row
                .status
                .as_deref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default();
            println!(
                "{mark} {}/{}  ({} · ctx {}k{status_note} · {key_status})",
                row.provider,
                row.model,
                row.api,
                row.context_window / 1000
            );
        }
        return Ok(());
    }

    if cli.mode == Mode::Json {
        // A headless boot another run's `bash` tool started is nested, and
        // clamps to that run. Read before the prompt: a run nested too
        // deep fails now rather than after draining stdin.
        let nested_boot = nested::NestedBoot::from_process_env()?;
        let sigterm_hit = Arc::new(AtomicBool::new(false));
        spawn_shutdown_signals(abort.clone(), sigterm_hit.clone());
        let prompt_arg = cli
            .prompt
            .clone()
            .or_else(|| cli.prompt_positional.clone())
            .unwrap_or_default();
        let prompt = if prompt_arg == "-" {
            tokio::select! {
                biased;
                _ = crate::agent::loop_::wait_for_abort(&abort) => {
                    flush_and_exit(if sigterm_hit.load(Ordering::Acquire) { 143 } else { 130 });
                }
                res = read_stdin() => res?,
            }
        } else {
            prompt_arg
        };
        if prompt.trim().is_empty() {
            bail!("empty prompt");
        }
        let agent = tokio::select! {
            biased;
            _ = crate::agent::loop_::wait_for_abort(&abort) => {
                flush_and_exit(if sigterm_hit.load(Ordering::Acquire) { 143 } else { 130 });
            }
            res = open_agent(&cli, abort.clone(), nested_boot) => res?,
        };
        if sigterm_hit.load(Ordering::Acquire) {
            flush_and_exit(143);
        }
        if abort.load(Ordering::Acquire) {
            flush_and_exit(130);
        }
        let res = json_mode::run_json(agent, prompt).await;
        if sigterm_hit.load(Ordering::Acquire) {
            if let Err(e) = &res {
                eprintln!("gg: {e:#}");
            }
            flush_and_exit(143);
        }
        return res;
    }
    if cli.prompt_positional.is_some() {
        bail!("a positional prompt is the --mode json spelling; print mode uses -p");
    }

    if cli.prompt.is_none() {
        // Interface choice mirrors pi (§15): a terminal runs the TUI;
        // a pipe is a mistake the user will want named, not a hang.
        if !std::io::stdin().is_terminal() {
            bail!("stdin is piped but no prompt was given; use gg -p \"…\" (or -p - to read it)");
        }
        let cwd = std::env::current_dir()?;
        let mut cfg = config::load()?;
        cfg.apply_cli(cli.provider.as_deref(), cli.model.as_deref())?;
        cfg.require_provider_for_base_url()?;
        cfg.apply_reasoning_cli(cli.reasoning_effort.as_deref(), cli.thinking_budget);
        cfg.apply_max_turns_cli(cli.max_turns);
        cfg.apply_max_seconds_cli(cli.max_seconds);
        let mut endpoint = cfg.active_endpoint();
        endpoint.require_auth()?;
        crate::llm::health::maybe_probe_local_health(&mut endpoint).await;
        report_fallback(&cfg, &endpoint);
        cfg.model = endpoint.model.clone();
        cfg.model_explicit = endpoint.model_explicit;
        cfg.context_window = endpoint.context_window;
        cfg.compact_threshold_tokens = endpoint.compact_threshold_tokens;
        if let Some(p) = cfg.providers.get_mut(&cfg.provider) {
            if !endpoint.model_explicit {
                p.default_model = endpoint.model.clone();
            }
            if !p.context_window_explicit {
                p.context_window = endpoint.context_window;
            }
            if !p.compact_threshold_explicit {
                p.compact_threshold_tokens = endpoint.compact_threshold_tokens;
            }
        }
        // Hooks (§22): the user's, then the launch directory's trusted
        // project files. Loaded once here for the first boot; the factory
        // reads them again for the directory each later session runs in.
        let (hooks, hook_warnings) = agent::hooks::load(&cwd, &cfg.trusted_projects, cli.no_hooks)?;
        for warning in &hook_warnings {
            eprintln!("warning: {warning}");
        }
        // Resolve the watch target before the boot: a failure here is a
        // startup message on stderr, not a warning behind the alternate
        // screen.
        let watch_cfg = if cli.watch {
            let watch = tui::watch::Config::discover(&cwd)?;
            eprintln!(
                "watching {} ({})",
                watch.root.display(),
                watch.profile.label()
            );
            Some(watch)
        } else {
            None
        };
        let (boot, factory) = boot_and_factory(&cli, &cfg, &cwd, abort.clone(), hooks)?;
        if let Some(path) = &boot.session_path {
            eprintln!("session: {}", path.display());
        }
        // A draft left beside the session by the process this run replaces.
        let draft = match (&watch_cfg, &boot.session_path) {
            (Some(_), Some(path)) => tui::watch::take_draft(path),
            _ => None,
        };
        // Shared discovery for /skill — the Agent discovers
        // its own copy at construction; same cwd, same result.
        let (skills, _) = agent::skills::discover(&cwd);
        let discovered = tui::Discovered {
            skills: std::sync::Arc::new(skills),
        };
        let exit = tui::run(boot, factory, discovered, cwd, cfg, abort, watch_cfg, draft).await?;
        return match exit {
            tui::Exit::Done(note) => {
                // The screen is the shell's again by now.
                if let Some(note) = note {
                    eprintln!("gg: {note}");
                }
                Ok(())
            }
            tui::Exit::Restart(plan) => {
                use std::os::unix::process::CommandExt as _;
                eprintln!("gg: restarting {}", plan.binary.display());
                // Only reachable once the terminal guard has dropped, so
                // the new process inherits a tty exactly as the shell left
                // it. `exec` replaces this process; it returns only on
                // failure.
                let err = std::process::Command::new(&plan.binary)
                    .args(&plan.args)
                    .exec();
                bail!("restarting {} failed: {err}", plan.binary.display());
            }
        };
    }

    // The nest, as in JSON mode: read before the prompt.
    let nested_boot = nested::NestedBoot::from_process_env()?;
    let sigterm_hit = Arc::new(AtomicBool::new(false));
    spawn_shutdown_signals(abort.clone(), sigterm_hit.clone());

    let prompt_arg = cli.prompt.clone().unwrap_or_default();
    let prompt = if prompt_arg == "-" {
        tokio::select! {
            biased;
            _ = crate::agent::loop_::wait_for_abort(&abort) => {
                flush_and_exit(if sigterm_hit.load(Ordering::Acquire) { 143 } else { 130 });
            }
            res = read_stdin() => res?,
        }
    } else {
        prompt_arg
    };
    if prompt.trim().is_empty() {
        bail!("empty prompt");
    }

    let mut agent = tokio::select! {
        biased;
        _ = crate::agent::loop_::wait_for_abort(&abort) => {
            flush_and_exit(if sigterm_hit.load(Ordering::Acquire) { 143 } else { 130 });
        }
        res = open_agent(&cli, abort.clone(), nested_boot) => res?,
    };
    if sigterm_hit.load(Ordering::Acquire) {
        flush_and_exit(143);
    }
    if abort.load(Ordering::Acquire) {
        flush_and_exit(130);
    }

    // Events flow through a channel to a drain task so progress renders
    // live on stderr while the run is still going. The drain owns the
    // printer and hands it back when the stream closes — the join is what
    // guarantees the final AssistantText has been consumed before the
    // answer is read (a race found the hard way in M2.11).
    let (mut tx, mut rx) = tokio::sync::mpsc::channel(64);
    let drain = tokio::spawn(async move {
        let mut printer = Printer::new().with_spinner(std::io::stderr().is_terminal());
        while let Some(event) = rx.recv().await {
            write_printer_lines(printer.event(event));
        }
        // The channel ended — a run that died mid-stream leaves the
        // spinner open with no further event to close it.
        write_printer_lines(printer.close());
        printer
    });

    let result = agent.run(prompt, &mut tx).await;
    drop(tx);
    let printer = drain.await.context("event renderer task panicked")?;
    if sigterm_hit.load(Ordering::Acquire) {
        if let Err(e) = &result {
            eprintln!("gg: {e:#}");
        } else if !printer.answer().is_empty() {
            let _ = writeln!(std::io::stdout(), "{}", printer.answer());
        }
        flush_and_exit(143);
    }
    result?;

    // A consumer that closed early (`… | head -c 5`) is a normal end for
    // a producer, not a failure — a failing answer write exits quietly.
    let _ = writeln!(std::io::stdout(), "{}", printer.answer());
    Ok(())
}

fn flush_and_exit(code: i32) -> ! {
    // `process::exit` runs no destructors, so the group kill `Jobs::drop`
    // would do happens here instead: a kept background job does not
    // outlive the process. The second pass catches a deadline that kept a
    // job while this ran — a second Ctrl-C can land mid-call.
    crate::agent::tools::bash::kill_jobs_at_exit();
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    crate::agent::tools::bash::kill_jobs_at_exit();
    std::process::exit(code);
}

/// Write one batch of printer lines: spinner lines start with '\r' and
/// are written raw — each update overwrites the last in place, the close
/// line carries its own newline; every other line is plain stderr.
fn write_printer_lines(lines: Vec<String>) {
    for line in lines {
        if line.starts_with('\r') {
            eprint!("{line}");
            std::io::stderr().flush().ok();
        } else {
            eprintln!("{line}");
        }
    }
}

/// Ctrl-C requests an abort; the loop stops between tool calls (§9) and
/// the run finishes cleanly with whatever it had. A second Ctrl-C forces
/// an immediate exit. On Unix, SIGTERM triggers a graceful abort and
/// sets `sigterm_hit` so the caller can exit 143 after flushing output.
fn spawn_shutdown_signals(abort: Arc<AtomicBool>, sigterm_hit: Arc<AtomicBool>) {
    let abort_c = abort.clone();
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if abort_c.swap(true, Ordering::AcqRel) {
                flush_and_exit(130);
            }
            eprintln!("aborting… (Ctrl-C again to force)");
        }
    });
    #[cfg(unix)]
    if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        tokio::spawn(async move {
            while sig.recv().await.is_some() {
                sigterm_hit.store(true, Ordering::Release);
                if abort.swap(true, Ordering::AcqRel) {
                    flush_and_exit(143);
                }
                eprintln!("received SIGTERM, shutting down gracefully…");
            }
        });
    }
}

async fn read_stdin() -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut buf = String::new();
    tokio::io::stdin()
        .read_to_string(&mut buf)
        .await
        .context("reading prompt from stdin")?;
    Ok(buf)
}

/// CLI flags → session pick. Both interfaces resolve flags through this
/// one mapping.
fn pick_of(cli: &Cli) -> tui::SessionPick {
    if let Some(path) = &cli.session_file {
        tui::SessionPick::Path(path.clone())
    } else if let Some(prefix) = &cli.resume {
        tui::SessionPick::Prefix(prefix.clone())
    } else if cli.r#continue {
        tui::SessionPick::Latest
    } else {
        tui::SessionPick::New
    }
}

/// Say when ranking passed over the first shipped default. Startup only,
/// and on stderr: stdout carries the answer, the table or the events. It
/// takes the endpoint as last resolved, after any local `/health` probe, so
/// it names the model in use.
fn report_fallback(cfg: &config::Config, endpoint: &config::Endpoint) {
    if let Some(note) = cfg.fallback_note(endpoint) {
        eprintln!("gg: {note}");
    }
}

/// Build the Agent print mode runs (every mode persists by default, §13;
/// -c/--resume replay an existing file). A nested boot clamps to the run
/// that started it, whatever its flags ask for.
async fn open_agent(
    cli: &Cli,
    abort: Arc<AtomicBool>,
    nested_boot: Option<nested::NestedBoot>,
) -> anyhow::Result<Agent> {
    let mut cfg = config::load()?;
    // A nested run without a model of its own takes its parent's current
    // one; `-P` alone names a provider, whose default then applies.
    let model = cli.model.clone().or_else(|| {
        cli.provider
            .is_none()
            .then(|| {
                nested_boot
                    .as_ref()
                    .and_then(|nest| nest.parent_model.clone())
            })
            .flatten()
    });
    cfg.apply_cli(cli.provider.as_deref(), model.as_deref())?;
    cfg.require_provider_for_base_url()?;
    cfg.apply_reasoning_cli(cli.reasoning_effort.as_deref(), cli.thinking_budget);
    cfg.apply_max_turns_cli(cli.max_turns);
    cfg.apply_max_seconds_cli(cli.max_seconds);
    if let Some(nest) = &nested_boot {
        cfg.max_turns = nest.effective_max_turns(cfg.max_turns);
        cfg.max_seconds = nest.effective_max_seconds(cfg.max_seconds);
        // A nested run's own commands take the longer ceiling: nobody
        // watches them, and the person-sized one still kills a long
        // compile.
        cfg.bash_timeout_secs = cfg.nested_bash_timeout_secs;
    }
    let mut endpoint = cfg.active_endpoint();
    endpoint.require_auth()?;
    crate::llm::health::maybe_probe_local_health(&mut endpoint).await;
    report_fallback(&cfg, &endpoint);
    let cwd = std::env::current_dir()?;
    // A nested run keeps its parent's hooks, on or off; which files it
    // reads is its own directory's.
    let no_hooks = match nested_boot.as_ref() {
        Some(nest) => {
            if cli.no_hooks {
                eprintln!("warning: --no-hooks ignored: a nested run keeps its parent's hooks");
            }
            nest.hooks_off
        }
        None => cli.no_hooks,
    };
    let (hooks, hook_warnings) = agent::hooks::load(&cwd, &cfg.trusted_projects, no_hooks)?;
    for warning in &hook_warnings {
        eprintln!("warning: {warning}");
    }
    let pick = pick_of(cli);
    let boot = assemble_boot(BootOpts {
        cfg: &cfg,
        endpoint: &endpoint,
        cwd: &cwd,
        abort,
        cli,
        pick,
        hooks: &hooks,
        nested_boot: nested_boot.as_ref(),
    })?;
    if let Some(path) = &boot.session_path {
        eprintln!("session: {}", path.display());
    }
    Ok(boot.agent)
}

/// The TUI's starting boot plus the factory that rebuilds agents for
/// `/new` and `/session`. One construction path for both interfaces.
fn boot_and_factory(
    cli: &Cli,
    cfg: &config::Config,
    cwd: &std::path::Path,
    abort: Arc<AtomicBool>,
    hooks: agent::hooks::Hooks,
) -> anyhow::Result<(tui::Boot, tui::Factory)> {
    let pick = pick_of(cli);
    let endpoint = cfg.active_endpoint();
    let boot = assemble_boot(BootOpts {
        cfg,
        endpoint: &endpoint,
        cwd,
        abort,
        cli,
        pick,
        hooks: &hooks,
        nested_boot: None,
    })?;

    // The factory rebuilds an agent for `/new` and `/resume` with a
    // one-shot cli: print mode, no prompt, no model or turn overrides —
    // the run's own pick of session, tools and hooks. A TUI boot never
    // nests, so it carries no nest. Hooks are read again per boot: a
    // session's are its directory's, project files included.
    let cfg = cfg.clone();
    let cli_tools = cli.tools.clone();
    let no_session = cli.no_session;
    let watch = cli.watch;
    let no_hooks = cli.no_hooks;
    let factory: tui::Factory = Arc::new(move |pick, cwd, endpoint, abort| {
        let cli = Cli {
            prompt: None,
            tools: cli_tools.clone(),
            r#continue: false,
            resume: None,
            no_session,
            mode: Mode::Print,
            prompt_positional: None,
            export: None,
            watch,
            provider: None,
            model: None,
            list_models: false,
            no_hooks,
            reasoning_effort: None,
            thinking_budget: None,
            max_turns: None,
            max_seconds: None,
            session_file: None,
        };
        let (hooks, _warnings) = agent::hooks::load(cwd, &cfg.trusted_projects, no_hooks)?;
        assemble_boot(BootOpts {
            cfg: &cfg,
            endpoint,
            cwd,
            abort,
            cli: &cli,
            pick,
            hooks: &hooks,
            nested_boot: None,
        })
    });
    Ok((boot, factory))
}

/// What one boot needs: the run's config and endpoint, the session pick,
/// the user's stop flag, and, for a headless boot another run started, the
/// nest it clamps to.
struct BootOpts<'a> {
    cfg: &'a config::Config,
    endpoint: &'a config::Endpoint,
    cwd: &'a std::path::Path,
    abort: Arc<AtomicBool>,
    cli: &'a Cli,
    pick: tui::SessionPick,
    hooks: &'a agent::hooks::Hooks,
    nested_boot: Option<&'a nested::NestedBoot>,
}

/// What resolving a session pick yields: the store to append through,
/// the entries to replay (resume), and the path for `/session`.
type ResolvedSession = (
    Option<agent::session::io::SessionStore>,
    Option<Vec<agent::session::Entry>>,
    Option<std::path::PathBuf>,
);

/// Build one [`tui::Boot`]: resolve the session (store + entries), then
/// construct the agent around it. The single path both interfaces and
/// the TUI factory share. `opts.nested_boot` is set by a headless boot
/// another run started.
fn assemble_boot(opts: BootOpts<'_>) -> anyhow::Result<tui::Boot> {
    let (store, entries, session_path) = open_session_for(
        opts.cli,
        opts.cwd,
        &opts.endpoint.label(),
        opts.pick.clone(),
        opts.nested_boot
            .and_then(|nest| nest.parent_session.as_deref()),
    )?;
    let mut agent = build_agent(&opts, store)?;
    if let Some(entries) = entries {
        agent.load_entries(entries);
    }
    if opts.cli.watch {
        // The restart resumes by id: the file must exist before the first
        // message, or the new process would find nothing to continue.
        agent.ensure_session_file()?;
    }
    Ok(tui::Boot {
        agent,
        session_path,
        sessions_disabled: opts.cli.no_session,
    })
}

/// Agent construction for one boot: config, client, registry filtered by
/// the allowlist, cwd, and the already-resolved store. The allowlist is
/// resolved against the canonical names so the nest a nested run's own
/// commands carry can be built before the registry is.
fn build_agent(
    opts: &BootOpts<'_>,
    store: Option<agent::session::io::SessionStore>,
) -> anyhow::Result<Agent> {
    let cfg = opts.cfg;
    let endpoint = opts.endpoint;
    let cwd = opts.cwd;
    let cli = opts.cli;
    let hooks = opts.hooks;
    let nested_boot = opts.nested_boot;
    let abort = opts.abort.clone();
    let client = LlmClient::from_endpoint(endpoint)?
        .with_retry(cfg.max_retries)
        .with_request_timeout(cfg.request_timeout_secs);
    let mut allowlist =
        config::resolve_tools(cli.tools.clone(), cfg.tools.clone(), agent::tools::NAMES)?;
    if let Some(nest) = nested_boot {
        allowlist = nest.narrow_tools(cli.tools.as_deref(), allowlist)?;
    }
    // What the runs this run's commands start inherit.
    let nest = nested::ParentEnv {
        depth: nested_boot.map_or(0, |nest| nest.depth),
        session: store.as_ref().map(|store| store.id().to_string()),
        model: Some(endpoint.label()),
        tool_cap: Some(allowlist.clone()),
        max_turns_cap: cfg.max_turns,
        max_seconds_cap: cfg.max_seconds,
        hooks_off: hooks.disabled(),
    }
    .shared();
    let mut registry =
        default_registry(cwd.to_path_buf(), cfg.bash_timeout_secs, Arc::clone(&nest));
    registry.retain(|t| allowlist.iter().any(|n| n == t.name()));

    Ok(Agent::new(
        client,
        registry,
        endpoint.model.clone(),
        endpoint.max_tokens,
        abort,
        cwd.to_path_buf(),
        store,
        // Interfaces stream (§15, M5): deltas render as they arrive. Only
        // the loop's scripted tests drive the non-streaming path.
        true,
        endpoint.context_window,
        hooks.clone(),
    )
    .with_replay_reasoning(endpoint.replay_reasoning)
    .with_compact_threshold(endpoint.compact_threshold_tokens)
    .with_max_turns(cfg.max_turns)
    .with_max_seconds(cfg.max_seconds)
    .with_model_label(endpoint.label())
    .with_parent_env(nest))
}

/// Resolve a session pick into (store, entries-to-replay, path). New:
/// fresh file, its header linked to `parent` when a run started it.
/// Resume: reopen the matched file and return its entries so the Agent
/// continues that conversation. Load warnings print to stderr — boots
/// happen before the TUI owns the screen.
fn open_session_for(
    cli: &Cli,
    cwd: &std::path::Path,
    model: &str,
    pick: tui::SessionPick,
    parent: Option<&str>,
) -> anyhow::Result<ResolvedSession> {
    use agent::session::io::{SessionFile, SessionStore};

    // A fresh store records the session that started it, when one did.
    fn linked(
        store: anyhow::Result<SessionStore>,
        parent: Option<&str>,
    ) -> anyhow::Result<SessionStore> {
        match parent {
            Some(parent) => store?.with_parent(Some(parent.to_string())),
            None => store,
        }
    }

    if cli.no_session {
        if !matches!(pick, tui::SessionPick::New) {
            anyhow::bail!("--no-session cannot resume");
        }
        return Ok((None, None, None));
    }
    let chosen = match pick {
        tui::SessionPick::New => {
            let store = linked(SessionStore::create(cwd, model), parent)?;
            let path = store.path().to_path_buf();
            return Ok((Some(store), None, Some(path)));
        }
        tui::SessionPick::Path(path) => {
            let resolved = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let existing_non_empty = match std::fs::metadata(&resolved) {
                Ok(m) => {
                    if m.is_dir() {
                        anyhow::bail!("--session-file {} is a directory", resolved.display());
                    }
                    m.len() > 0
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("reading metadata for {}", resolved.display()));
                }
            };
            if existing_non_empty {
                let (_, mut entries, warnings) = agent::session::io::load(&resolved)?;
                for warning in warnings {
                    eprintln!("warning: {warning}");
                }
                let mut store = SessionStore::open_existing(&resolved)?;
                let repaired =
                    agent::session::io::repair_dangling_tool_calls(&mut entries, &mut store)?;
                if repaired > 0 {
                    eprintln!(
                        "warning: repaired {repaired} interrupted tool call(s) from the previous run"
                    );
                }
                return Ok((Some(store), Some(entries), Some(resolved)));
            }
            let store = linked(
                SessionStore::create_at(resolved.clone(), cwd, model),
                parent,
            )?;
            return Ok((Some(store), None, Some(resolved)));
        }
        tui::SessionPick::Latest => {
            let dir = agent::session::io::sessions_dir(cwd)
                .context("no HOME: nowhere to look for sessions")?;
            agent::session::io::latest_in(&dir)
        }
        tui::SessionPick::Prefix(prefix) => {
            let dir = agent::session::io::sessions_dir(cwd)
                .context("no HOME: nowhere to look for sessions")?;
            let (sessions, _) = agent::session::io::list_sessions(&dir);
            Some(agent::session::io::resolve_prefix(&sessions, &prefix)?)
        }
    };
    let chosen: SessionFile =
        chosen.ok_or_else(|| anyhow::anyhow!("no sessions yet for this directory"))?;
    let (_, mut entries, warnings) = agent::session::io::load(&chosen.path)?;
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    let mut store = SessionStore::open_existing(&chosen.path)?;
    // A crash mid-batch leaves assistant tool_calls without results; §8
    // forbids replaying those. Repair before the conversation continues.
    let repaired = agent::session::io::repair_dangling_tool_calls(&mut entries, &mut store)?;
    if repaired > 0 {
        eprintln!("warning: repaired {repaired} interrupted tool call(s) from the previous run");
    }
    let path = chosen.path.clone();
    Ok((Some(store), Some(entries), Some(path)))
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        Cli::try_parse_from(std::iter::once("gg").chain(args.iter().copied()))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn watch_is_a_tui_only_mode() {
        assert!(parse(&["--watch", "-p", "x"]).is_err());
        assert!(parse(&["--watch", "--export", "abc"]).is_err());
        let cli = parse(&["--watch"]).unwrap();
        assert!(cli.watch);
    }

    #[test]
    fn session_flags_conflict_according_to_the_rules() {
        // Resume selection is one choice; no-session excludes both.
        assert!(parse(&["-p", "x", "-c", "--resume", "abc"]).is_err());
        assert!(parse(&["-p", "x", "-c", "--no-session"]).is_err());
        assert!(parse(&["-p", "x", "--resume", "abc", "--no-session"]).is_err());

        let cli = parse(&["-p", "x", "-c"]).unwrap();
        assert!(cli.r#continue && cli.resume.is_none() && !cli.no_session);
        let cli = parse(&["-p", "x", "--resume", "abc"]).unwrap();
        assert!(!cli.r#continue && cli.resume.as_deref() == Some("abc"));
    }

    #[test]
    fn provider_model_and_list_models_flags_parse() {
        let cli = parse(&["-P", "deepseek", "-m", "deepseek-v4-pro", "-p", "hi"]).unwrap();
        assert_eq!(cli.provider.as_deref(), Some("deepseek"));
        assert_eq!(cli.model.as_deref(), Some("deepseek-v4-pro"));

        let list = parse(&["--list-models"]).unwrap();
        assert!(list.list_models);
        assert!(parse(&["--list-models", "-p", "hi"]).is_err());
        assert!(parse(&["--list-models", "--watch"]).is_err());

        let r = parse(&[
            "--reasoning-effort",
            "low",
            "--thinking-budget",
            "2048",
            "-p",
            "hi",
        ])
        .unwrap();
        assert_eq!(r.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(r.thinking_budget, Some(2048));

        let w2 = parse(&[
            "--max-turns",
            "40",
            "--session-file",
            "/tmp/s.jsonl",
            "-p",
            "hi",
        ])
        .unwrap();
        assert_eq!(w2.max_turns, Some(40));
        let w3 = parse(&["--max-seconds", "0", "-p", "hi"]).unwrap();
        assert_eq!(w3.max_seconds, Some(0));
        assert_eq!(
            w2.session_file.as_deref(),
            Some(std::path::Path::new("/tmp/s.jsonl"))
        );
        assert!(parse(&["--session-file", "/tmp/s.jsonl", "-c", "-p", "hi"]).is_err());
        assert!(parse(&["--session-file", "/tmp/s.jsonl", "--no-session", "-p", "hi"]).is_err());
    }
}
