//! The list's half of [`State`](super::super::State): the listing cache
//! the rows are built from, whether the list is open and from what text,
//! which directories and rows the session list shows, which row is
//! highlighted, and what its keys do. The rows, the filter and the hint
//! are [`picker`](crate::tui::picker)'s; the trigger is its.

use crate::agent::session::io::SessionFile;
use crate::tui::picker::{Action, List, Pick, Picker, Row, matching, trigger};

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

    /// The `/model` list's rows, built from the state's model rows:
    /// one row per model, in the state's order.
    pub(crate) fn model_list(&self) -> Picker {
        let rows = self
            .models
            .iter()
            .map(|m| crate::tui::picker::Row::Model {
                suggestion: crate::tui::picker::model_row(m, &self.active_model),
                search: format!("{}/{}", m.provider, m.model).to_lowercase(),
            })
            .collect();
        crate::tui::picker::Picker::for_models(rows)
    }

    /// The `/skill` list's rows, built from the state's skill set: one
    /// row per skill whose name has no whitespace (the typed command
    /// splits at the first space), in the state's order.
    pub(crate) fn skill_list(&self) -> Picker {
        let rows = self
            .skills
            .iter()
            .filter(|skill| !skill.name.chars().any(char::is_whitespace))
            .map(|skill| {
                let suggestion = crate::tui::picker::skill_row(skill);
                let search =
                    format!("{} {}", suggestion.label, suggestion.description).to_lowercase();
                crate::tui::picker::Row::Skill { suggestion, search }
            })
            .collect();
        crate::tui::picker::Picker::for_skills(rows)
    }

    /// Open this trigger's list: the sessions from the listing cache, the
    /// models from the state's rows, the skills from the state's skill
    /// set; on the trigger's filter.
    fn open_picker(&mut self) {
        let Some(trigger) = trigger(self.editor.text()) else {
            return;
        };
        let mut pick = match trigger.list {
            crate::tui::picker::List::Sessions => self.session_list(),
            crate::tui::picker::List::Models => self.model_list(),
            crate::tui::picker::List::Skills => self.skill_list(),
        };
        let filter = trigger.filter.clone();
        let selected = self.default_row(&trigger, &pick);
        pick.filter = filter;
        pick.selected = selected;
        self.picker = Some(pick);
    }

    /// The list's starting highlight: the active model's row while the
    /// `/model` list's filter is empty, the first row otherwise.
    fn default_row(&self, trigger: &crate::tui::picker::Trigger, pick: &Picker) -> usize {
        if trigger.filter.is_empty() && trigger.list == crate::tui::picker::List::Models {
            pick.model_rows()
                .iter()
                .position(|row| row.label() == Some(self.active_model.as_str()))
                .unwrap_or(0)
        } else {
            0
        }
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
        self.picker.as_ref().is_some_and(|pick| {
            if pick.list == List::Sessions {
                pick.rows().is_empty()
            } else {
                pick.model_rows().is_empty()
            }
        })
    }

    /// The rows the open list's filter keeps, in listing order: the
    /// sessions with, above them, the matching action rows when a filter
    /// is typed; the models as they stand. Empty with the list closed,
    /// and when the filter matches nothing. A listing is about a hundred
    /// rows and this runs once per frame, so the kept rows are cloned,
    /// not borrowed.
    pub fn picker_matches(&self) -> Vec<Row> {
        let Some(pick) = &self.picker else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        if pick.list == List::Sessions {
            if !pick.filter.is_empty() {
                rows.extend(
                    [Action::CopyPath, Action::Info]
                        .into_iter()
                        .map(Row::Action),
                );
            }
            rows.extend(pick.rows().iter().map(|row| Row::Session(row.clone())));
        } else {
            // The `/model` and `/skill` lists have no matching action
            // rows: their rows stand as they were built.
            rows.extend(pick.model_rows().iter().cloned());
        }
        matching(&rows, &pick.filter).into_iter().cloned().collect()
    }

    /// Whether the list shows every directory's sessions.
    pub fn session_scope_all(&self) -> bool {
        self.picker.as_ref().is_some_and(Picker::all_directories)
    }

    /// What the open list's hint row says of its own keys.
    pub(in crate::tui) fn picker_hint_keys(&self) -> crate::tui::picker::HintKeys {
        match self.picker.as_ref() {
            Some(pick) if pick.list == List::Sessions => crate::tui::picker::HintKeys::Sessions {
                every_directory: self.session_scope_all(),
            },
            Some(pick) if pick.list == List::Models => crate::tui::picker::HintKeys::Complete {
                enter: "Enter switches",
            },
            Some(pick) if pick.list == List::Skills => crate::tui::picker::HintKeys::Complete {
                enter: "Enter runs",
            },
            _ => crate::tui::picker::HintKeys::Complete {
                enter: "Enter opens",
            },
        }
    }

    /// Why the open list has no row to show, as its panel's own row
    /// says it: the session list's reasons where it is the sessions,
    /// the model list's where it is the models, the skill list's where
    /// it is the skills.
    pub fn picker_empty_reason(&self) -> String {
        let Some(pick) = self.picker.as_ref() else {
            return String::new();
        };
        if pick.list == List::Models {
            let filter = pick.filter.trim();
            if filter.is_empty() {
                "no models to pick".to_string()
            } else {
                format!("no model matches {filter:?}")
            }
        } else if pick.list == List::Skills {
            let filter = pick.filter.trim();
            if filter.is_empty() {
                "no skills found".to_string()
            } else {
                format!("no skill matches {filter:?}")
            }
        } else {
            self.session_empty_reason()
        }
    }

    /// Why the open session list has no row to show, as its panel's own
    /// row says it. A run that keeps no sessions, or a missing HOME, is
    /// said before a scan that is still to come: neither ever brings a
    /// listing.
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

    /// Tab in the `/session` list: the scope narrows from every
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

    /// Enter: what the highlighted row does: the session it names, the
    /// action row's action, or the model row's command. The list stays
    /// open, filter and all: Enter never discards it, so a failed pick
    /// leaves the user where they were, with the panel's error row
    /// saying why. The caller closes it once the action is done (an open
    /// session closes it of its own, through `new_session`); `None` when
    /// the list is closed, or when the filter keeps no row — which the
    /// panel's empty row says, so nothing goes to the transcript.
    pub fn picker_action(&mut self) -> Option<Pick> {
        self.sync_picker();
        let selected = self.picker.as_ref()?.selected;
        let rows = self.picker_matches();
        let row = rows.get(selected)?;
        match row {
            Row::Session(row) => Some(Pick::Open(row.session.clone())),
            Row::Action(action) => match action {
                Action::CopyPath => Some(Pick::CopyPath),
                Action::Info => Some(Pick::Info),
            },
            Row::Model { suggestion, .. } => Some(Pick::Command(suggestion.completion.clone())),
            Row::Skill { suggestion, .. } => {
                // The trigger the composer's text still gives: the list
                // is open, so it does, and its arguments are the typed
                // ones.
                let args = trigger(self.editor.text()).map_or(String::new(), |t| t.args);
                Some(Pick::Command(format!("{}{}", suggestion.completion, args)))
            }
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

    /// Tab in the `/model` or `/skill` list: the highlighted row's
    /// completion in the composer, the list open still — `provider/model`
    /// is the filter's own text, and `/skill <name> ` leaves the typed
    /// arguments after the name, ready to be completed further.
    pub fn picker_tab(&mut self) {
        let Some(text) = self
            .picker_matches()
            .get(self.picker.as_ref().map_or(0, |pick| pick.selected))
            .and_then(|row| match row {
                Row::Model { suggestion, .. } => Some(suggestion.completion.clone()),
                Row::Skill { suggestion, .. } => {
                    let args = trigger(self.editor.text()).map_or(String::new(), |t| t.args);
                    Some(format!("{}{}", suggestion.completion, args))
                }
                _ => None,
            })
        else {
            return;
        };
        self.editor.set_text(&text);
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
        // list opens (even when it would be empty: an empty list is a
        // panel row, not a transcript line). The sessions ask for a
        // scan; the models read nothing.
        if self
            .picker
            .as_ref()
            .is_none_or(|open| open.list != trigger.list)
        {
            if trigger.list == crate::tui::picker::List::Sessions {
                self.request_session_scan();
            }
            self.open_picker();
            return;
        }
        // A changed filter is a new list: the highlight returns to its
        // default row and the last failed pick goes with the old one. The
        // rows themselves, and the session list's scope, stay.
        let Some(pick) = self.picker.as_ref() else {
            return;
        };
        if pick.filter == trigger.filter {
            return;
        }
        let selected = self.default_row(&trigger, pick);
        if let Some(pick) = self.picker.as_mut() {
            pick.filter = trigger.filter;
            pick.selected = selected;
            pick.error = None;
        }
    }
}
