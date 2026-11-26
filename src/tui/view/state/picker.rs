//! The list's half of [`State`](super::super::State): whether a list is
//! open and from what text, its rows, which is highlighted, and what its
//! keys do. The rows, the filter and the hint are
//! [`picker`](crate::tui::picker)'s; the session listing is
//! [`sessions`](super::sessions)'s.

use crate::tui::picker::{Action, List, Pick, Picker, Row, matching, trigger};

impl crate::tui::view::State {
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
