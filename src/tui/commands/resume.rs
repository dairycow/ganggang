//! `/resume`: the session picker's listing, the scope `Tab` toggles, the
//! number or prefix a typed `/resume` names, and opening the chosen
//! session in the directory it continues in. [`handle_command`](super::handle_command)
//! keeps the arms; the logic lives here so `commands.rs` stays small.

use super::CmdCtx;

/// A bare `/resume`: every project's sessions open in the picker, which
/// is where they are read, filtered and chosen, so none of them is
/// printed into the transcript. This project's (`project_dir` is where
/// they are filed) are kept apart for Tab. A store with nothing in it is
/// still one line there, and so is one that holds only the session gg is
/// in now: the picker never lists a session it could not move into.
fn open_picker(
    state: &mut super::super::view::State,
    sessions: Vec<crate::agent::session::io::SessionFile>,
    project_dir: &std::path::Path,
    home: Option<std::path::PathBuf>,
) {
    if sessions.is_empty() {
        // Nothing to pick from: no picker, and no listing for a number
        // to count.
        state.resume = None;
        state.pending_resume.clear();
        state.push_info("no sessions yet");
        return;
    }
    // The order `/resume <n>` counts is whatever listing the picker shows.
    if !state.open_resume(sessions, project_dir, home) {
        // The session in use was the only one: nothing else to pick
        // from, so no picker and no listing for a number to count.
        state.resume = None;
        state.pending_resume.clear();
        state.push_info("no other session yet");
    }
}

/// The listing a bare `/resume` asked for, read off the loop: the
/// picker's rows are built here, on the loop, from what the scan found.
/// A store with nothing in it, and one that holds only the session in
/// use, are `open_picker`'s to say (`no sessions yet`, `no other
/// session yet`). A run in flight, or composer text that is not the
/// filter but a draft typed while the store was read, keeps the user
/// where they are: the listing is theirs to ask for again, not to be
/// opened over their draft. Every warning the scan kept is pushed as-is:
/// a file it could not read is absent from the picker, and the
/// transcript says so instead of the row just missing.
pub(in crate::tui) fn resume_listed(
    state: &mut super::super::view::State,
    sessions: Vec<crate::agent::session::io::SessionFile>,
    warnings: Vec<String>,
    cwd: &std::path::Path,
) {
    use crate::agent::session::io;
    if state.busy || !state.editor.text().trim().is_empty() {
        state.push_info("sessions are listed: /resume again to pick one");
        return;
    }
    for warning in warnings {
        state.push_warning(&warning);
    }
    let Some(project) = io::sessions_dir(cwd) else {
        return;
    };
    open_picker(state, sessions, &project, crate::home::dir());
}

/// Tab in the `/resume` picker: the scope narrows from every project's
/// sessions to this project's, or widens back. Both listings were read
/// when the picker opened, so this reads nothing.
pub(in crate::tui) fn toggle_resume_scope(state: &mut super::super::view::State) {
    if state.resume_all_projects() {
        state.resume_narrow();
    } else {
        state.resume_widen();
    }
}

/// `/resume <prefix>`, under the user's sessions root.
pub(super) fn resolve_session_prefix(
    cwd: &std::path::Path,
    prefix: &str,
) -> anyhow::Result<crate::agent::session::io::SessionFile> {
    let root = crate::agent::session::io::sessions_root()
        .ok_or_else(|| anyhow::anyhow!("no HOME: nowhere to look for sessions"))?;
    resolve_prefix_everywhere(&root, cwd, prefix)
}

/// `/resume <prefix>`: the prefix names a session by its uuid or by the
/// 8-hex id in its file name. This project's sessions (under `root`, in
/// the directory `cwd`'s slug names) are searched first; when none of
/// them matches, every project's are, so an id the all-projects view
/// shows can be typed from here.
fn resolve_prefix_everywhere(
    root: &std::path::Path,
    cwd: &std::path::Path,
    prefix: &str,
) -> anyhow::Result<crate::agent::session::io::SessionFile> {
    use crate::agent::session::io;
    let (here, _) = io::list_sessions(&root.join(io::list::slug(cwd)));
    if io::matching_prefix(&here, prefix).is_empty() {
        let (every, _) = io::list_all_sessions(root);
        return io::resolve_prefix(&every, prefix);
    }
    io::resolve_prefix(&here, prefix)
}

/// Open one session of a listing, by its file, in the directory it
/// continues in: its own when it still has one, which moves gg there.
/// The transcript says which, unless gg was already in it. Errors are
/// the caller's to report.
pub(in crate::tui) fn resume_picked(
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

#[cfg(test)]
mod tests;
