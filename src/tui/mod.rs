//! The interactive interface (spec §15): fullscreen transcript, editor,
//! and footer. One agent task owns the [`Agent`] for the whole session;
//! the UI event loop exchanges inputs and [`UiEvent`]s with it over
//! channels and never touches the network itself. Ctrl+G lends the
//! terminal to an external editor (`external`) while the loop keeps
//! running behind it.

pub mod agent_task;
pub mod clipboard;
pub mod commands;
pub mod composer;
pub mod external;
pub mod footer;
pub mod history;
pub mod input;
pub mod keys;
pub mod layout;
pub mod line;
pub mod mentions;
pub mod reader;
pub mod shell;
pub mod suggest;
pub mod term;
pub mod theme;
pub mod tick;
pub mod transcript;
pub mod view;
pub mod watch;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::agent::skills::Skill;
use agent_task::{AgentInput, UiEvent, agent_task};
use clipboard::osc52_sequence;
use commands::{CmdCtx, Command, Submission, classify_submission, handle_command};
use external::Outcome;
use history::Arrow;
use keys::{KeyAction, WHEEL_ROWS, Wheel, classify, classify_wheel};
use reader::KeyReader;
use shell::spawn_shell;
use tick::Tick;
use view::{Cancel, State, render};

/// Lines per PageUp/PageDown press. Half-a-screen would be ideal but the
/// loop does not know the height at key time; a fixed step pages through
/// anything, just in bigger or smaller bites.
const SCROLL_STEP: u16 = 10;

/// A fresh agent plus what `/session` needs to say about it. Built by the
/// factory in main (which owns config); the TUI never touches config.
pub struct Boot {
    pub agent: Agent,
    pub session_path: Option<PathBuf>,
    /// `--no-session`: `/resume` is disabled for this run.
    pub sessions_disabled: bool,
}

/// Which session a new [`Boot`] should carry.
pub enum SessionPick {
    New,
    Latest,
    Prefix(String),
    Path(PathBuf),
}

/// Rebuilds an Agent for `/new` and `/resume`. The TUI owns the lifetime
/// decisions; main owns construction (config, client, registry).
pub type Factory = Arc<
    dyn Fn(SessionPick, &crate::config::Endpoint, Arc<AtomicBool>) -> anyhow::Result<Boot>
        + Send
        + Sync,
>;

/// The discovery sets the TUI renders lists from, shared read-only with
/// the command handler. The Agent discovers its own copy of the skills
/// (same cwd, same result); `/skills`, `/skill:`, and `/<template>` all
/// answer without a round-trip to the agent task.
pub struct Discovered {
    pub skills: Arc<Vec<Skill>>,
    /// Prompt templates (M7.4) — `/<name>` inserts the body.
    pub prompts: Arc<Vec<crate::prompts::PromptTemplate>>,
    /// User-level hooks (§22) — `/hooks` lists them.
    pub hooks: Arc<crate::agent::hooks::Hooks>,
}

/// Why the TUI loop ended. `Restart` carries the exec plan for a `--watch`
/// rebuild; `main` runs it only after this function has returned and the
/// terminal guard has put the tty back.
pub enum Exit {
    Done(Option<String>),
    Restart(watch::Restart),
}

/// Run the TUI until the user quits. `first` is consumed by the first
/// agent task; `/new` and `/resume` rebuild agents through `factory`.
/// What comes back is for the user, to read once the screen is the shell's
/// again (it is: screen and guard drop, in that order, on the way out).
///
/// `watch` enables dev rebuilds; `draft` is composer text carried over
/// from the process a restart replaced.
// One flat list, like [`Agent::new`]: each parameter has a distinct owner
// (boot, lifetime, discovery, location, limits, transport, dev loop) and
// a builder would only hide the same decisions behind punctuation.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    first: Boot,
    factory: Factory,
    discovered: Discovered,
    cwd: std::path::PathBuf,
    config: crate::config::Config,
    abort: Arc<AtomicBool>,
    watch_cfg: Option<watch::Config>,
    draft: Option<String>,
) -> anyhow::Result<Exit> {
    let Discovered {
        skills,
        prompts,
        hooks,
    } = discovered;
    let _guard = term::Guard::new()?;
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal =
        term::Screen::new(backend).map_err(|e| anyhow::anyhow!("initializing terminal: {e}"))?;

    let mut active_endpoint = config.active_endpoint();
    let initial_endpoint = active_endpoint.clone();
    let boot = first;
    // Prompt history, read once: the loop must not re-read a file it is
    // also appending to, and a load that skipped lines is worth one line
    // of feedback before anything else is on the screen.
    let history = history::History::load();
    let mut state = State::new(
        &cwd,
        &active_endpoint.label(),
        active_endpoint.context_window,
        skills.clone(),
        prompts.clone(),
        history,
    );
    if let Some(warning) = state.history.warning() {
        state.push_warning(&warning);
    }
    state.session_path = boot.session_path.clone();
    state.sessions_disabled = boot.sessions_disabled;
    // A resumed session shows its past immediately, not after the first
    // new turn (the agent is about to move into the task — read first).
    state.seed_from_entries(boot.agent.entries());
    if let Some(draft) = draft {
        state.editor.set_text(&draft);
    }

    // `--watch`: a build task reports on a channel. Without it, a channel
    // whose sender is kept alive and never used keeps the select arm idle.
    let (never_tx, never_rx) = mpsc::channel::<watch::Event>(1);
    let _keep_watch_channel_open = never_tx;
    let (watch_handle, mut watch_rx) = match &watch_cfg {
        Some(cfg) => {
            state
                .footer
                .set_watch(Some(format!("watch {} · watching", cfg.profile.label())));
            let (handle, rx) = watch::spawn(cfg.clone());
            (Some(handle), Some(rx))
        }
        None => (None, Some(never_rx)),
    };
    let original_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // A build that succeeded but could not restart yet (run in flight, or
    // unsent text). `Some(true)` is a Ctrl+R restart, which carries the
    // draft; `Some(false)` is automatic and waits for an empty composer.
    let mut rebuild_ready: Option<bool> = None;
    let mut restart: Option<watch::Restart> = None;

    let (mut input_tx, input_rx) = mpsc::channel::<AgentInput>(16);
    let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(256);
    tokio::spawn(agent_task(boot.agent, input_rx, ui_tx.clone()));
    let mut keys = KeyReader::spawn();
    // Ctrl+G sessions report here. The loop keeps its sender, so the
    // channel only ever speaks when an editor session ended.
    let (editor_tx, mut editor_rx) = mpsc::channel::<Outcome>(1);
    // Set while the external editor owns the screen: nothing is drawn, but
    // agent events keep folding into the transcript, so a run in flight
    // neither stalls nor bursts on return.
    let mut editor: Option<external::Running> = None;
    let mut tick = Tick::new();
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    #[cfg(not(unix))]
    let mut sigterm = ();

    loop {
        // A build finished earlier and the loop has been waiting for a safe
        // moment: no run in flight, no editor, and (for an automatic
        // restart) nothing typed that would be lost.
        if let Some(manual) = rebuild_ready {
            let draft_empty = state.editor.text().trim().is_empty();
            if watch::can_restart(state.busy, editor.is_some(), draft_empty, manual) {
                match &watch_cfg {
                    Some(cfg) => match watch::restart_plan(
                        cfg,
                        &cwd,
                        state.session_path.as_deref(),
                        state.editor.text(),
                        &original_args,
                    ) {
                        Ok(mut plan) => {
                            if active_endpoint != initial_endpoint {
                                plan.args = watch::restart::apply_endpoint_args(
                                    plan.args,
                                    &active_endpoint.provider,
                                    &active_endpoint.model,
                                );
                            }
                            restart = Some(plan);
                            break;
                        }
                        Err(e) => {
                            rebuild_ready = None;
                            state
                                .footer
                                .set_watch(Some("restart failed · Ctrl+R".into()));
                            state.push_warning(&format!("cannot restart: {e:#}"));
                        }
                    },
                    None => rebuild_ready = None,
                }
            }
        }
        if editor.is_none() {
            terminal
                .draw(|frame| render(frame, &mut state))
                .map_err(|e| anyhow::anyhow!("drawing: {e}"))?;
        }

        // The spinner's clock, in line with the footer before the loop
        // waits: armed while a run is live, disarmed the moment it ends.
        tick.sync(tokio::time::Instant::now(), state.spinning());
        tokio::select! {
            // The footer's spinner. Armed only while a run is live, so an
            // idle view never wakes — and never redraws — on a timer; and
            // a fixed deadline rather than a fresh wait each pass, so a
            // stream of deltas cannot keep pushing the next frame back.
            _ = until(tick.deadline()) => {
                state.tick_spinner();
                tick.fired(tokio::time::Instant::now());
            }
            key = keys.recv() => {
                let Some(event) = key else { break };
                match event {
                    crossterm::event::Event::Key(key) => match classify(key) {
                        // Ctrl+C (gap §5): never an unconditional quit —
                        // the run is asked to stop, a draft is cleared, and
                        // only a third kind of press ends the TUI. The rule
                        // itself is [`State::interrupt`]; what the view
                        // cannot do (set the abort flag, leave the loop) is
                        // done here. A hard exit mid-run is safe by
                        // construction: the session file flushes per append
                        // (§13).
                        KeyAction::Interrupt => match state.interrupt() {
                            // All this does to the agent is set the flag:
                            // the running tool notices it (GAP 3) and the
                            // loop reports the run as aborted.
                            Cancel::Stopping => {
                                abort.store(true, Ordering::Release);
                                state.push_info("stopping — Ctrl+C again to quit");
                            }
                            Cancel::ForceQuit => {
                                abort.store(true, Ordering::Release);
                                break;
                            }
                            Cancel::DraftCleared => {
                                state.push_info("draft cleared — Ctrl+C again to quit")
                            }
                            Cancel::Quit => break,
                        },
                        // Esc keeps its meaning: the run's abort key, and it
                        // marks the run stopping so the footer shows the
                        // cancel took. Idle, it does nothing.
                        KeyAction::Abort if state.busy => {
                            abort.store(true, Ordering::Release);
                            state.set_stopping(true);
                        }
                        KeyAction::CopyAnswer => copy_last_answer(&mut state),
                        // Ctrl+R: a rebuild request from the user. The
                        // build runs in the watch task; its success comes
                        // back as `Event::Built { manual: true }`.
                        KeyAction::Rebuild => match (&watch_cfg, &watch_handle) {
                            (None, _) => {
                                state.push_info("Ctrl+R rebuilds and restarts in --watch mode")
                            }
                            (Some(_), Some(_)) if state.busy => {
                                state.push_info("busy — Ctrl+R once the run finishes")
                            }
                            (Some(_), Some(_)) if editor.is_some() => {
                                state.push_info("Ctrl+R waits for the external editor to close")
                            }
                            (Some(_), Some(handle)) => {
                                rebuild_ready = None;
                                if handle.rebuild() {
                                    state.footer.set_watch(Some("⚙ building…".into()));
                                } else {
                                    state.push_info("a build is already running");
                                }
                            }
                            _ => {}
                        },
                        // The key reader parked on delivering this key
                        // (see `reader`); a session's end wakes it.
                        KeyAction::ExternalEditor => {
                            match external::Session::prepare(state.editor.text()) {
                                Ok(session) => {
                                    // As the shell would have handed the
                                    // terminal over (see `term::lend`).
                                    if let Err(e) = term::lend() {
                                        state.push_warning(&format!(
                                            "external editor: {e:#}"
                                        ));
                                    }
                                    editor = Some(session.launch(editor_tx.clone()));
                                }
                                Err(e) => {
                                    state.push_warning(&format!("external editor: {e:#}"));
                                    keys.resume();
                                }
                            }
                        }
                        // Submissions land between runs: the text stays
                        // in the editor for resubmission, with a line
                        // saying why nothing happened.
                        KeyAction::Send if state.busy => {
                            state.push_info(if state.stopping() {
                                "stopping — the run is still winding down; your input stays in the editor"
                            } else {
                                "busy — a run is in flight; your input stays in the editor"
                            });
                        }
                        KeyAction::Send if !state.busy => {
                            let Some(text) = state.editor.take_submission() else {
                                continue;
                            };
                            if text.trim().is_empty() {
                                continue;
                            }
                            match classify_submission(&text) {
                                Submission::Plain(prompt) => {
                                    // Everything the editor hands over is
                                    // history — prompts, shell lines,
                                    // commands — except the way out, which
                                    // a next session has no use for.
                                    state.history.record(&text);
                                    // `@path` mentions (M7.5) carry file
                                    // content to the model; the transcript
                                    // keeps the readable line the user
                                    // typed, the model gets the
                                    // attachments.
                                    let expanded = mentions::expand_mentions(&prompt, &cwd);
                                    state.push_user(&prompt);
                                    state.begin_run();
                                    if input_tx
                                        .send(AgentInput::Prompt(expanded))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Submission::Shell { command, to_context } => {
                                    state.history.record(&text);
                                    state.begin_run();
                                    spawn_shell(command, cwd.clone(), to_context, ui_tx.clone());
                                }
                                Submission::Command(command) => {
                                    if !matches!(command, Command::Quit) {
                                        state.history.record(&text);
                                    }
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
                                    match handle_command(command, ctx).await {
                                        Ok(false) => {}
                                        Ok(true) => break,
                                        Err(e) => state.push_command_error(&format!("{e:#}")),
                                    }
                                }
                            }
                        }
                        KeyAction::Newline => state.editor.newline(),
                        KeyAction::Backspace => state.editor.backspace(),
                        KeyAction::Insert(c) => state.editor.insert_char(c),
                        KeyAction::LineStart => state.editor.move_home(),
                        KeyAction::LineEnd => state.editor.move_end(),
                        KeyAction::KillToEnd => state.editor.kill_to_end(),
                        KeyAction::KillToStart => state.editor.kill_to_start(),
                        KeyAction::CursorLeft => state.editor.move_left(),
                        KeyAction::CursorRight => state.editor.move_right(),
                        KeyAction::ToggleExpand => state.toggle_expand(),
                        KeyAction::ToggleThinking => state.toggle_thinking(),
                        KeyAction::ScrollUp => state.scroll_up(SCROLL_STEP),
                        KeyAction::ScrollDown => state.scroll_down(SCROLL_STEP),
                        // ↑/↓ belong to the suggestion popup while it is
                        // open; otherwise they recall history (from an empty
                        // or single-line composer) or move between the
                        // composer's lines — the rule itself is
                        // [`history::history_for`].
                        KeyAction::SuggestPrev => match state.arrow(true) {
                            Arrow::SuggestPrev => state.suggest_prev(),
                            Arrow::Prev => state.recall_prev(),
                            Arrow::LineUp => state.editor.move_up(),
                            // ↓'s half of the table: not reachable from ↑.
                            Arrow::SuggestNext | Arrow::Next | Arrow::LineDown | Arrow::Nothing => {}
                        },
                        KeyAction::SuggestNext => match state.arrow(false) {
                            Arrow::SuggestNext => state.suggest_next(),
                            Arrow::Next => state.recall_next(),
                            Arrow::LineDown => state.editor.move_down(),
                            Arrow::Nothing => {}
                            // ↑'s half of the table: not reachable from ↓.
                            Arrow::SuggestPrev | Arrow::Prev | Arrow::LineUp => {}
                        },
                        // Ctrl+P / Ctrl+N: history whatever the composer
                        // holds, a three-line draft included (readline's
                        // muscle memory). While the popup is open the arrows
                        // are its, so these keys stand down.
                        KeyAction::HistoryPrev if !state.suggest_open() => state.recall_prev(),
                        KeyAction::HistoryNext if !state.suggest_open() => state.recall_next(),
                        KeyAction::SuggestComplete if state.suggest_open() => {
                            state.complete_suggestion()
                        }
                        _ => {}
                    },
                    // Bracketed paste: the whole payload is editor text —
                    // pasted newlines are newlines, never submissions.
                    crossterm::event::Event::Paste(text) => {
                        state.editor.insert_paste(&text);
                    }
                    // The mouse is for the wheel, and for nothing else: a
                    // press, a drag, or a hold is ignored outright (see
                    // [`classify_wheel`]), so a stray click can never move a
                    // cursor or a selection.
                    crossterm::event::Event::Mouse(mouse) => match classify_wheel(mouse) {
                        Some(Wheel::Up) => state.scroll_up(WHEEL_ROWS),
                        Some(Wheel::Down) => state.scroll_down(WHEEL_ROWS),
                        None => {}
                    },
                    _ => {}
                }
            }
            ui = ui_rx.recv() => {
                let Some(event) = ui else { break };
                if !agent_task::apply(event, &mut state, &mut input_tx).await {
                    break;
                }
            }
            _ = wait_sigterm(&mut sigterm) => {
                abort.store(true, Ordering::Release);
                break;
            }
            event = next_watch_event(&mut watch_rx) => {
                let Some(event) = event else {
                    // The build task died (it only ends by itself when the
                    // loop is gone). Keep the session alive without it.
                    watch_rx = None;
                    state.footer.set_watch(None);
                    state.push_warning("watch task ended unexpectedly; --watch is off");
                    continue;
                };
                match event {
                    watch::Event::Building => {
                        rebuild_ready = None;
                        state.footer.set_watch(Some("⚙ building…".into()));
                    }
                    watch::Event::Failed { tail, .. } => {
                        rebuild_ready = None;
                        state
                            .footer
                            .set_watch(Some("build failed · Ctrl+R".into()));
                        state.push_warning(&format!("cargo build failed:\n{tail}"));
                    }
                    watch::Event::Built { manual } => {
                        let idle = !state.busy && editor.is_none();
                        let draft_empty = state.editor.text().trim().is_empty();
                        if watch::can_restart(state.busy, editor.is_some(), draft_empty, manual) {
                            let Some(cfg) = &watch_cfg else { continue };
                            match watch::restart_plan(
                                cfg,
                                &cwd,
                                state.session_path.as_deref(),
                                state.editor.text(),
                                &original_args,
                            ) {
                                Ok(mut plan) => {
                                    if active_endpoint != initial_endpoint {
                                        plan.args = watch::restart::apply_endpoint_args(
                                            plan.args,
                                            &active_endpoint.provider,
                                            &active_endpoint.model,
                                        );
                                    }
                                    restart = Some(plan);
                                    break;
                                }
                                Err(e) => {
                                    state
                                        .footer
                                        .set_watch(Some("restart failed · Ctrl+R".into()));
                                    state.push_warning(&format!("cannot restart: {e:#}"));
                                }
                            }
                        } else {
                            rebuild_ready = Some(manual);
                            state
                                .footer
                                .set_watch(Some("rebuild ready · Ctrl+R".into()));
                            if !idle {
                                state.push_info("new build ready — restarting when idle");
                            } else if !manual {
                                state.push_info(
                                    "new build ready — Ctrl+R to restart; your draft is kept",
                                );
                            }
                        }
                    }
                }
            }
            Some(outcome) = editor_rx.recv(), if editor.is_some() => {
                editor = None;
                // Tty and terminal modes back as the shell had them (a
                // killed editor leaves its own behind), then in again.
                if let Some(not_put_right) = term::reclaim()
                    .map_err(|e| {
                        anyhow::anyhow!("taking the terminal back from the external editor: {e}")
                    })?
                {
                    state.push_warning(&format!("external editor: {not_put_right}"));
                }
                term::repaint(&mut terminal)?;
                keys.resume();
                match outcome {
                    Outcome::Saved(text) => state.editor.set_text(&text),
                    Outcome::Unchanged(why) => {
                        state.push_warning(&format!("external editor {why}"))
                    }
                    Outcome::Notice(text) => state.push_info(&text),
                }
            }
        }
    }
    // An editor still open here means the agent task died under it.
    let note = external::conclude(editor.take()).await?;
    match restart {
        Some(plan) => Ok(Exit::Restart(plan)),
        None => Ok(Exit::Done(note)),
    }
}

/// Wait for the spinner's next frame; a disarmed clock never comes due.
async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Wait for SIGTERM on Unix; pends forever if unavailable.
#[cfg(unix)]
async fn wait_sigterm(sig: &mut Option<tokio::signal::unix::Signal>) {
    match sig {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn wait_sigterm(_sig: &mut ()) {
    std::future::pending::<()>().await;
}

/// The watch channel as a select arm that idles forever once the task is
/// gone (and the arm has been disarmed by the caller).
async fn next_watch_event(rx: &mut Option<mpsc::Receiver<watch::Event>>) -> Option<watch::Event> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Ctrl+X: copy the last assistant answer via OSC 52 (terminal-native
/// clipboard, no dependencies). Feedback lands in the transcript either
/// way — silent clipboards are indistinguishable from broken ones.
fn copy_last_answer(state: &mut State) {
    let Some(answer) = state.last_answer() else {
        state.push_warning("nothing to copy yet");
        return;
    };
    let sequence = osc52_sequence(&answer);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .and_then(|mut tty| std::io::Write::write_all(&mut tty, sequence.as_bytes()));
    match written {
        Ok(()) => state.push_info(&format!("copied {} chars", answer.chars().count())),
        Err(e) => state.push_warning(&format!("cannot write to /dev/tty: {e}")),
    }
}

#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;
