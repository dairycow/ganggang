//! The session picker's half of [`State`](super::super::State): the
//! listing cache the rows are built from, whether the list is open and
//! from what text, which directories and rows it shows, which row is
//! highlighted, and what its keys do. The rows, the filter and the hint
//! are [`picker`](crate::tui::picker)'s; the trigger is its.

use crate::agent::session::io::SessionFile;
use crate::tui::picker::{Pick, Picker, Row, matching, trigger};

impl crate::tui::view::State {
    /// The session list's rows, built from the state's listing cache: the
    /// session gg is in now is dropped (reopening it would only reload it
    /// from disk), and the sessions filed under `sessions.dir` are this
    /// directory's. Every directory's scope, an empty filter and the top
    /// row highlighted, as a list opens; even when nothing is left, since
    /// an empty list is a panel row now, not a transcript line.
    fn session_list(&self) -> Picker {
        let sessions: Vec<SessionFile> = self
            .sessions
            .everywhere
            .iter()
            .filter(|session| Some(&session.path) != self.session_path.as_ref())
            .cloned()
            .collect();
        let here: Vec<SessionFile> = if self.sessions.dir.is_some() {
            sessions
                .iter()
                .filter(|session| {
                    self.sessions
                        .dir
                        .as_ref()
                        .is_some_and(|dir| session.path.parent() == Some(dir.as_path()))
                })
                .cloned()
                .collect()
        } else {
            sessions.clone()
        };
        Picker::new(here, sessions, self.sessions.home.as_deref())
    }

    /// Open the session list from the listing cache, on the trigger's
    /// filter: not the whole composer text.
    fn open_picker_from_listing(&mut self) {
        let mut pick = self.session_list();
        pick.filter = trigger(self.editor.text()).map_or_else(String::new, |t| t.filter);
        self.picker = Some(pick);
    }

    /// Whether the list is open.
    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Whether the open list's scope holds no row at all: an empty
    /// directory in the narrow view, or an empty store in the wide one.
    /// The panel draws one row of its own then, because the transcript,
    /// which the panel covers, would otherwise say nothing newer.
    pub fn session_scope_empty(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|pick| pick.rows().is_empty())
    }

    /// The rows the open list's filter keeps, in listing order: empty
    /// with the list closed, and when the filter matches nothing. A
    /// listing is about a hundred rows and this runs once per frame, so
    /// the kept rows are cloned, not borrowed.
    pub fn picker_matches(&self) -> Vec<Row> {
        match &self.picker {
            Some(pick) => {
                let rows: Vec<Row> = pick
                    .rows()
                    .iter()
                    .map(|row| Row::Session(row.clone()))
                    .collect();
                matching(&rows, &pick.filter).into_iter().cloned().collect()
            }
            None => Vec::new(),
        }
    }

    /// Whether the list shows every directory's sessions.
    pub fn session_scope_all(&self) -> bool {
        self.picker.as_ref().is_some_and(Picker::all_directories)
    }

    /// Why the open list has no row to show, as its panel's own row says
    /// it. A run that keeps no sessions, or a missing HOME, is said before
    /// a scan that is still to come: neither ever brings a listing.
    pub fn session_empty_reason(&self) -> String {
        let filter = self.picker.as_ref().map_or("", |pick| pick.filter.trim());
        if self.sessions_disabled {
            "sessions are disabled for --no-session runs".to_string()
        } else if self.sessions.dir.is_none() {
            "no HOME: nowhere to look for sessions".to_string()
        } else if !self.sessions.loaded {
            "reading sessions…".to_string()
        } else if self.session_scope_empty() && self.session_scope_all() {
            "no sessions yet".to_string()
        } else if self.session_scope_empty() {
            "no sessions in this directory".to_string()
        } else {
            format!("no session matches {filter:?}")
        }
    }

    /// Tab: every directory's sessions. The filter stays, the highlight
    /// returns to the top row, and a failed open's error goes with the
    /// listing it was reported against.
    pub fn session_scope_all_directories(&mut self) {
        if let Some(pick) = self.picker.as_mut() {
            pick.widen();
        }
    }

    /// Tab again: this directory's sessions, the filter kept, the
    /// highlight back on the top row.
    pub fn session_scope_this_directory(&mut self) {
        if let Some(pick) = self.picker.as_mut() {
            pick.narrow();
        }
    }

    /// Ask for a scan of the session store. `--no-session` runs have no
    /// store to scan, so their requests stay away.
    pub fn request_session_scan(&mut self) {
        if !self.sessions_disabled {
            self.sessions.wanted = true;
        }
    }

    /// A scan is owed: one was requested, none is in flight, and there is
    /// a store to scan (a missing HOME is the panel's to say).
    pub fn session_scan_ready(&self) -> bool {
        self.sessions.dir.is_some()
            && self.sessions.wanted
            && !self.sessions.scanning
            && !self.sessions_disabled
    }

    /// The loop asked for the scan: mark it in flight until the listing
    /// is stored. A request made while one is in flight is kept in
    /// `wanted` and runs when that scan returns.
    pub fn session_scan_started(&mut self) {
        self.sessions.wanted = false;
        self.sessions.scanning = true;
    }

    /// The listing a finished scan found, stored on the cache: the rows
    /// the list is built from, marked loaded, and the scan's warnings,
    /// which reach the transcript when they differ from the previous
    /// scan's: the same file the scan could not read is not said twice.
    pub fn sessions_listed(&mut self, sessions: Vec<SessionFile>, warnings: Vec<String>) {
        let differs = warnings != self.sessions.warnings;
        self.sessions.everywhere = sessions;
        self.sessions.loaded = true;
        self.sessions.scanning = false;
        self.sessions.warnings = warnings;
        if differs {
            let warnings = self.sessions.warnings.clone();
            for warning in &warnings {
                self.push_warning(warning);
            }
        }
        // A listing that arrived while the list is open is its new rows,
        // with the user's scope and filter kept, and the highlight on the
        // session it stood on (by path, still in the new rows) or on the
        // first row when that session is gone. The error goes with the
        // rows it was reported against.
        if let Some(open) = &self.picker {
            let (every_directory, filter) = (open.all_directories(), open.filter.clone());
            let was_on = self.picker_matches().get(open.selected).cloned();
            let mut pick = self.session_list();
            if every_directory {
                pick.widen();
            } else {
                pick.narrow();
            }
            pick.filter = filter;
            self.picker = Some(pick);
            // Where the row it stood on is now, among the rows the kept
            // filter still shows; the first row when it is gone.
            let at = was_on.as_ref().and_then(|was| {
                self.picker_matches()
                    .iter()
                    .position(|row| row.same_row(was))
            });
            if let (Some(pick), Some(at)) = (self.picker.as_mut(), at) {
                pick.selected = at;
            }
        }
    }

    /// Tab in the `/resume` list: the scope narrows from every
    /// directory's sessions to this directory's, or widens back. Both
    /// listings were read when the list opened, so this reads nothing.
    pub fn toggle_session_scope(&mut self) {
        if self.session_scope_all() {
            self.session_scope_this_directory();
        } else {
            self.session_scope_all_directories();
        }
    }

    /// ↑: the highlight climbs, stopping at the top.
    pub fn picker_prev(&mut self) {
        self.move_highlight(true, 1);
    }

    /// ↓: the highlight descends, stopping at the last matching row.
    pub fn picker_next(&mut self) {
        self.move_highlight(false, 1);
    }

    fn move_highlight(&mut self, up: bool, by: usize) {
        self.sync_picker();
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

    /// Enter: what the highlighted row does. The list stays open, filter
    /// and all: Enter never discards it, so a failed open leaves the user
    /// where they were, with the panel's error row saying why. The caller
    /// closes it once the session is open (or the list closes of its own
    /// Esc); `None` when the list is closed, or when the filter keeps no
    /// row — which the panel's empty row says, so nothing goes to the
    /// transcript.
    pub fn picker_action(&mut self) -> Option<Pick> {
        self.sync_picker();
        let selected = self.picker.as_ref()?.selected;
        let rows = self.picker_matches();
        let row = rows.get(selected)?;
        match row {
            Row::Session(_) => Some(Pick::Open(row.session().clone())),
        }
    }

    /// A failed open: the transcript keeps the error for when the list
    /// closes, and the open list's panel shows it now, because the panel
    /// covers the transcript's newest rows — where the error sits.
    pub fn pick_failed(&mut self, error: &str) {
        self.push_command_error(error);
        if let Some(pick) = self.picker.as_mut() {
            pick.error = Some(error.to_string());
        }
    }

    /// Esc: the list closes, and the filter goes with it.
    pub fn close_picker(&mut self) {
        self.picker = None;
        self.editor.clear();
    }

    /// The list is the trigger's: the composer's text decides whether it
    /// is open at all. No trigger drops the open list, and a trigger with
    /// no open list opens one from the cache (even when the cache is
    /// empty: an empty list is a panel row, not a transcript line) and
    /// asks for a scan. A changed filter is a new list: the highlight
    /// returns to the top row and the last failed open goes with the old
    /// one. render() calls this every frame, and so does every key the
    /// list handles.
    pub(crate) fn sync_picker(&mut self) {
        let Some(trigger) = trigger(self.editor.text()) else {
            self.picker = None;
            return;
        };
        // No open list, or one for another kind of list: this trigger's
        // list opens from the cache (even when the cache is empty: an
        // empty list is a panel row, not a transcript line) and asks for
        // a scan.
        if self
            .picker
            .as_ref()
            .is_none_or(|open| open.list != trigger.list)
        {
            self.open_picker_from_listing();
            self.request_session_scan();
        }
        let Some(pick) = self.picker.as_mut() else {
            return;
        };
        if pick.filter != trigger.filter {
            pick.filter = trigger.filter;
            pick.selected = 0;
            pick.error = None;
        }
    }
}
