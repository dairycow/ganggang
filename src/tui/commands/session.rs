//! `/session`: the session scan the list is built from, read off the
//! loop, opening the chosen session in the directory it continues in,
//! and the one-line actions `copy path` and `info` run on the current
//! session. The list itself opens from the composer's text as it is
//! typed; [`handle_command`](super::handle_command) keeps the arms, and
//! the logic lives here so `commands.rs` stays small.

use super::CmdCtx;
use crate::tui::view::State;

/// The `/session` list's store scan, read off the loop: it reads every
/// file of every directory, so it runs on a blocking thread, and the
/// listing comes back as `UiEvent::SessionsListed`, which the loop
/// answers with [`sessions_listed`].
pub(in crate::tui) fn spawn_session_scan(
    ui_tx: tokio::sync::mpsc::Sender<super::super::agent_task::UiEvent>,
) {
    let Some(root) = crate::agent::session::io::sessions_root() else {
        return;
    };
    tokio::spawn(async move {
        let listed = tokio::task::spawn_blocking(move || {
            crate::agent::session::io::list_all_sessions(&root)
        })
        .await;
        // A scan that failed is reported too, as the listing's one warning:
        // the loop marks the scan finished only when its event arrives.
        let (sessions, warnings) = match listed {
            Ok(listed) => listed,
            Err(e) => (Vec::new(), vec![format!("cannot list sessions: {e:#}")]),
        };
        let _ = ui_tx
            .send(super::super::agent_task::UiEvent::SessionsListed { sessions, warnings })
            .await;
    });
}

/// The listing a finished scan found, stored on the state's cache
/// ([`State::sessions_listed`](super::super::view::State::sessions_listed)):
/// the list's rows are built from it, whether or not the list is open.
/// Every warning the scan kept that the previous scan did not is pushed
/// as-is: a file it could not read is absent from the list, and the
/// transcript says so instead of the row just missing.
pub(in crate::tui) fn sessions_listed(
    state: &mut super::super::view::State,
    sessions: Vec<crate::agent::session::io::SessionFile>,
    warnings: Vec<String>,
) {
    state.sessions_listed(sessions, warnings);
}

/// Open one session of a listing, by its file, in the directory it
/// continues in: its own when it still has one, which moves gg there.
/// The transcript says which, unless gg was already in it. Errors are
/// the caller's to report.
pub(in crate::tui) fn open_session(
    session: crate::agent::session::io::SessionFile,
    ctx: CmdCtx<'_>,
) -> anyhow::Result<()> {
    let CmdCtx {
        state,
        factory,
        input_tx,
        ui_tx,
        active_endpoint,
        abort,
        cwd,
        ..
    } = ctx;
    let continuation = continuation(&session, cwd);
    let boot = factory(
        super::super::SessionPick::Path(session.path.clone()),
        &continuation.directory,
        active_endpoint,
        abort.clone(),
    )?;
    super::swap_agent(boot, state, input_tx, ui_tx, &continuation.directory);
    *cwd = continuation.directory;
    if let Some(note) = continuation.note {
        state.push_info(&note);
    }
    Ok(())
}

/// Where a session continues when it opens, and what the transcript says
/// about it.
struct Continuation {
    directory: std::path::PathBuf,
    note: Option<String>,
}

/// A session continues in the directory it started in, when there still
/// is one, wherever gg was launched: its tools, its `AGENTS.md` and
/// skills, and the `@mentions` and `!shell` lines typed after it are that
/// directory's. A directory that is gone (a worktree removed) leaves the
/// session continuing in the directory gg is working in, and the
/// transcript says so. A header that names no directory says nothing.
fn continuation(
    session: &crate::agent::session::io::SessionFile,
    current: &std::path::Path,
) -> Continuation {
    let stays = std::path::PathBuf::from(current);
    if session.cwd.is_empty() {
        return Continuation {
            directory: stays,
            note: None,
        };
    }
    let own = std::path::Path::new(&session.cwd);
    if !own.is_absolute() || !own.is_dir() {
        return Continuation {
            directory: stays,
            note: Some(format!(
                "the directory this session started in ({}) is gone; it continues in {}",
                session.cwd,
                current.display()
            )),
        };
    }
    let note = (own != current).then(|| format!("this session runs in {}", own.display()));
    Continuation {
        directory: own.to_path_buf(),
        note,
    }
}

/// The current session's one-line block: where its file is, which model
/// it runs, and the run's totals. `--no-session` runs have no file to
/// point at, and a fresh session's file is written only on its first
/// message, which the path line says.
pub(in crate::tui) fn session_info(state: &mut State, model_label: &str) {
    let path = match state.session_path.as_ref() {
        Some(path) => {
            let mut shown = path.display().to_string();
            if !path.exists() {
                shown.push_str(" (not written until the first message)");
            }
            shown
        }
        None => "not persisted (--no-session)".to_string(),
    };
    state.push_info(&format!(
        "session: {path}\nmodel: {model_label}\nentries: {}\ntokens: {} in / {} out",
        state.entries, state.tokens_in, state.tokens_out,
    ));
}

/// The list's `copy path` row on the current session: its `.jsonl`
/// file's path, copied through `write` (the loop passes
/// [`clipboard::copy`](super::super::clipboard::copy), a test passes a
/// recorder). A run without a session file has nothing to copy, and a
/// write that fails is the terminal's problem, reported as an error.
pub(in crate::tui) fn copy_session_path(
    state: &mut State,
    write: impl Fn(&str) -> std::io::Result<()>,
) -> Result<(), String> {
    let path = state
        .session_path
        .as_ref()
        .ok_or_else(|| "no session file to copy: this run is --no-session".to_string())?;
    let path_text = path.display().to_string();
    if let Err(e) = write(&path_text) {
        return Err(format!("cannot write to /dev/tty: {e}"));
    }
    state.push_info(&format!("copied {path_text}"));
    Ok(())
}

#[cfg(test)]
mod tests;
