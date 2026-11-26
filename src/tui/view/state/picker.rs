//! The session picker's half of [`State`](super::super::State): whether
//! it is open, which directories and rows it shows, which row is
//! highlighted, and what its keys do. The rows, the filter and the hint
//! are [`picker`](crate::tui::picker)'s, and the composer's text is the
//! filter.

use std::path::PathBuf;

use crate::agent::session::io::SessionFile;
use crate::tui::picker::{Picker, SessionRow, matching};

impl crate::tui::view::State {
    /// Open the picker over the whole session store (newest first). It
    /// shows every project's sessions; the ones filed under `project_dir`
    /// are this project's, which Tab narrows it to. The composer is the
    /// filter: whatever it holds narrows the rows at once. The session
    /// gg is in now is dropped from the rows: reopening it would only
    /// reload it from disk, and it is the row Enter would open by
    /// default. `false` when that leaves nothing to open on, in which
    /// case the picker is closed and no listing remembered.
    pub fn open_picker(
        &mut self,
        sessions: Vec<SessionFile>,
        project_dir: &std::path::Path,
        home: Option<PathBuf>,
    ) -> bool {
        let sessions: Vec<SessionFile> = sessions
            .into_iter()
            .filter(|session| Some(&session.path) != self.session_path.as_ref())
            .collect();
        if sessions.is_empty() {
            self.picker = None;
            self.pending_resume.clear();
            return false;
        }
        // This project's rows are built from copies: every session goes
        // into the all-projects rows, this project's included.
        let here: Vec<SessionFile> = sessions
            .iter()
            .filter(|session| session.path.parent() == Some(project_dir))
            .cloned()
            .collect();
        let mut pick = Picker::new(here, sessions, home.as_deref());
        pick.filter = self.editor.text().to_string();
        self.picker = Some(pick);
        self.remember_listing();
        true
    }

    /// Whether the picker is open.
    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Whether the open picker's scope holds no row at all: an empty
    /// directory in the narrow view, or an empty store in the wide one.
    /// The panel draws one row of its own then, because the transcript,
    /// which the panel covers, would otherwise say nothing newer.
    pub fn session_scope_empty(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|pick| pick.rows().is_empty())
    }

    /// The rows the composer's text keeps, in listing order: empty with
    /// the picker closed, and when the filter matches nothing.
    pub fn picker_matches(&self) -> Vec<&SessionRow> {
        match &self.picker {
            Some(pick) => matching(pick.rows(), self.editor.text()),
            None => Vec::new(),
        }
    }

    /// Whether the picker shows every project's sessions.
    pub fn session_scope_all(&self) -> bool {
        self.picker.as_ref().is_some_and(Picker::all_directories)
    }

    /// Tab: every project's sessions. The filter stays, the highlight
    /// returns to the top row, and `/resume <n>` counts the wider listing.
    pub fn session_scope_all_directories(&mut self) {
        let Some(pick) = self.picker.as_mut() else {
            return;
        };
        pick.widen();
        self.remember_listing();
    }

    /// Tab again: this project's sessions, the filter kept.
    pub fn session_scope_this_directory(&mut self) {
        let Some(pick) = self.picker.as_mut() else {
            return;
        };
        pick.narrow();
        self.remember_listing();
    }

    /// `/resume <n>` counts whichever listing the picker shows, newest
    /// first and unfiltered.
    fn remember_listing(&mut self) {
        let Some(pick) = &self.picker else {
            return;
        };
        let listing = pick.rows().iter().map(|row| row.session.clone()).collect();
        self.pending_resume = listing;
    }

    /// ↑: the highlight climbs, stopping at the top.
    pub fn picker_prev(&mut self) {
        self.move_highlight(true, 1);
    }

    /// ↓: the highlight descends, stopping at the last matching row.
    pub fn picker_next(&mut self) {
        self.move_highlight(false, 1);
    }

    /// The mouse wheel: the highlight moves `by` rows, up or down. The
    /// panel's window follows it, so the highlighted row is always one
    /// of the rows on screen.
    pub fn scroll_picker(&mut self, up: bool, by: u16) {
        self.move_highlight(up, usize::from(by));
    }

    fn move_highlight(&mut self, up: bool, by: usize) {
        self.sync_resume();
        let last = self.picker_matches().len().saturating_sub(1);
        let Some(pick) = self.picker.as_mut() else {
            return;
        };
        pick.selected = if up {
            pick.selected.saturating_sub(by)
        } else {
            pick.selected.saturating_add(by).min(last)
        };
    }

    /// Enter: the highlighted session, as a clone for the loop to open.
    /// The picker stays open, filter and all: Enter never discards it, so
    /// a failed open leaves the user where they were, with the panel's
    /// error row saying why. The caller closes it once the session is
    /// open (or the picker closes of its own Esc); `None` when the
    /// picker is closed, or when the filter keeps no row — which the
    /// panel's empty row says, so nothing goes to the transcript.
    pub fn picker_highlighted(&mut self) -> Option<SessionFile> {
        self.sync_resume();
        self.picker_matches()
            .get(self.picker.as_ref()?.selected)
            .map(|row| row.session.clone())
    }

    /// A failed open: the transcript keeps the error for when the picker
    /// closes, and the open picker's panel shows it now, because the
    /// panel covers the transcript's newest rows — where the error sits.
    pub fn pick_failed(&mut self, error: &str) {
        self.push_command_error(error);
        if let Some(pick) = self.picker.as_mut() {
            pick.error = Some(error.to_string());
        }
    }

    /// Esc: the picker closes, and the filter goes with it.
    pub fn close_picker(&mut self) {
        self.picker = None;
        self.editor.clear();
    }

    /// The highlight belongs to the filter it was built for: a changed
    /// composer text is a new list, and the top row is the default
    /// again. render() calls this every frame, and so does every key
    /// the picker handles.
    pub(crate) fn sync_resume(&mut self) {
        let Some(pick) = self.picker.as_mut() else {
            return;
        };
        if pick.filter != self.editor.text() {
            pick.filter = self.editor.text().to_string();
            pick.selected = 0;
            // A changed filter is a new list: the last failed open
            // belonged to the old one.
            pick.error = None;
        }
    }
}
