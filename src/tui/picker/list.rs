//! One row of the list ([`Row`]), what `Enter` does on it ([`Pick`]),
//! the open list ([`Picker`]) and the filter that keeps its rows
//! ([`matching`]). What opens a list, and the rows' builders, are the
//! module's; a session's row is
//! [`SessionRow`](super::sessions::SessionRow)'s.

use std::path::Path;

use ratatui::text::Line as UiLine;

use super::super::suggest::{Suggestion, suggestion_row};
use super::super::theme::Palette;
use super::action_row;
use super::sessions::{SessionRow, row_line};
use super::{Action, List};
use crate::agent::session::io::SessionFile;

/// One row of the list, with its own text a filter has to appear in.
/// The session rows' is their [`SessionRow::haystack`]; the action
/// rows' is their label; a model row's is its label and description.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Row {
    /// A session's row, as the panel draws it.
    Session(SessionRow),
    /// An action row, beside the sessions: it shows above them while a
    /// filter is typed, and its search text is its label.
    Action(Action),
    /// A model row, as the `/model` panel draws it: the suggestion as
    /// the popup's row builders draw it, and the row's search text.
    Model {
        suggestion: Suggestion,
        search: String,
    },
    /// A skill row: the suggestion as the popup's row builders draw it
    /// (its search text is its name and description), and its completion
    /// is `/skill <name> `.
    Skill {
        suggestion: Suggestion,
        search: String,
    },
}

impl Row {
    /// Lower-cased text a filter word has to appear in.
    pub fn search_text(&self) -> &str {
        match self {
            Row::Session(row) => &row.haystack,
            Row::Action(action) => action.label(),
            Row::Model { search, .. } => search,
            Row::Skill { search, .. } => search,
        }
    }

    /// The row's first string: the `provider/model` of a model row, the
    /// action's label, a skill's name, `None` for a session (its many
    /// strings have no single first one). What a list's highlight is
    /// kept on, across a changed listing.
    pub fn label(&self) -> Option<&str> {
        match self {
            Row::Model { suggestion, .. } => Some(&suggestion.label),
            Row::Action(action) => Some(action.label()),
            Row::Skill { suggestion, .. } => Some(&suggestion.label),
            Row::Session(_) => None,
        }
    }

    /// The row as the panel draws it.
    pub fn line(&self, palette: &Palette, selected: bool, width: usize) -> UiLine<'static> {
        match self {
            Row::Session(row) => row_line(palette, row, selected, width),
            Row::Action(action) => suggestion_row(palette, &action_row(*action), selected, width),
            Row::Model { suggestion, .. } => suggestion_row(palette, suggestion, selected, width),
            Row::Skill { suggestion, .. } => suggestion_row(palette, suggestion, selected, width),
        }
    }

    /// Whether both rows are the same thing: what keeps the highlight
    /// across a refresh.
    pub fn same_row(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Session(a), Row::Session(b)) => a.session.path == b.session.path,
            (Row::Action(a), Row::Action(b)) => a == b,
            (Row::Model { suggestion: a, .. }, Row::Model { suggestion: b, .. }) => {
                a.label == b.label
            }
            (Row::Skill { suggestion: a, .. }, Row::Skill { suggestion: b, .. }) => {
                a.label == b.label
            }
            _ => false,
        }
    }
}

/// What `Enter` would do on the highlighted row.
#[derive(Debug, Clone)]
pub enum Pick {
    /// Open the session.
    Open(SessionFile),
    /// Copy the current session's `.jsonl` path to the clipboard.
    CopyPath,
    /// Show the current session's path, model, entries and tokens.
    Info,
    /// Run the highlighted row's command: `/model provider/model` or
    /// `/skill <name> [args]`.
    Command(String),
}

impl PartialEq for Pick {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Pick::Open(a), Pick::Open(b)) => a.path == b.path,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}

/// An open list: which rows it shows, which of them is highlighted, the
/// filter text the highlight was last built for, and the last failed
/// action's error. A changed filter is a new list, so the highlight
/// returns to the top row (the rule the suggestion popup keeps in its own
/// `suggest_filter`); a scope change or a changed filter clears the
/// error, which belongs to the rows and the filter it was reported
/// against.
#[derive(Debug, Clone)]
pub struct Picker {
    /// The kind of list this open list is.
    pub list: List,
    pub selected: usize,
    pub filter: String,
    /// The last failed open's message, for the panel's own row: the
    /// panel covers the transcript, where the error is also kept for
    /// when the list closes. `None` until one fails, and cleared by a
    /// scope change or a changed filter, which are a new list.
    pub error: Option<String>,
    /// This directory's sessions, newest first. The `/model` and `/skill`
    /// lists have no stored rows: here is empty for them.
    here: Vec<SessionRow>,
    /// Every directory's sessions, newest first, this directory's
    /// included. Empty for the `/model` list.
    everywhere: Vec<SessionRow>,
    /// Whether the list shows every directory's sessions.
    all_directories: bool,
    /// The `/model` list's rows, in the state's order, as its rows are
    /// built: `Row::Model`s. Empty for the session list.
    model_rows: Vec<Row>,
}

impl Picker {
    /// The session list over two listings of the same store, each newest
    /// first: this directory's sessions (`here`) and every directory's
    /// (`everywhere`, which includes the first). It opens showing every
    /// directory's, each row aged against the clock now and its directory
    /// abbreviated against `home`.
    pub fn new(here: Vec<SessionFile>, everywhere: Vec<SessionFile>, home: Option<&Path>) -> Self {
        let now = unix_now();
        Self {
            here: here
                .into_iter()
                .map(|session| SessionRow::new(session, now))
                .collect(),
            everywhere: everywhere
                .into_iter()
                .map(|session| SessionRow::anywhere(session, now, home))
                .collect(),
            all_directories: true,
            selected: 0,
            filter: String::new(),
            error: None,
            list: List::Sessions,
            model_rows: Vec::new(),
        }
    }

    /// The `/model` list over its `rows`: no session rows, the first row
    /// highlighted, no filter, no error.
    pub fn for_models(rows: Vec<Row>) -> Self {
        Self::for_rows(rows, List::Models)
    }

    /// The `/skill` list over its `rows`: no session rows, the first row
    /// highlighted, no filter, no error.
    pub fn for_skills(rows: Vec<Row>) -> Self {
        Self::for_rows(rows, List::Skills)
    }

    fn for_rows(rows: Vec<Row>, list: List) -> Self {
        Self {
            here: Vec::new(),
            everywhere: Vec::new(),
            all_directories: true,
            selected: 0,
            filter: String::new(),
            error: None,
            list,
            model_rows: rows,
        }
    }

    #[cfg(test)]
    pub(in crate::tui) fn for_test(rows: Vec<SessionRow>) -> Self {
        Self {
            here: rows,
            everywhere: Vec::new(),
            all_directories: false,
            selected: 0,
            filter: String::new(),
            error: None,
            list: List::Sessions,
            model_rows: Vec::new(),
        }
    }

    /// The `/model` list's rows in view, in the state's order; the
    /// `/skill` list's the same. Empty for the session list.
    pub fn model_rows(&self) -> &[Row] {
        &self.model_rows
    }

    /// The session rows in view, in listing order. Empty for the
    /// `/model` and `/skill` lists.
    pub fn rows(&self) -> &[SessionRow] {
        if self.all_directories {
            &self.everywhere
        } else {
            &self.here
        }
    }

    /// Whether the list shows every directory's sessions.
    pub fn all_directories(&self) -> bool {
        self.all_directories
    }

    /// Every directory's sessions. The filter stays, and the highlight
    /// returns to the top row: it is a new list, and the last failed
    /// open belonged to the old one.
    pub fn widen(&mut self) {
        self.all_directories = true;
        self.selected = 0;
        self.error = None;
    }

    /// This directory's sessions. The filter stays, the highlight returns
    /// to the top row, and a failed open's error goes with the listing it
    /// was reported against.
    pub fn narrow(&mut self) {
        self.all_directories = false;
        self.selected = 0;
        self.error = None;
    }
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The rows `filter` keeps, in order: every whitespace-separated word
/// appears in the row's search text, case-insensitively. An empty filter
/// keeps them all.
pub fn matching<'a>(rows: &'a [Row], filter: &str) -> Vec<&'a Row> {
    let words: Vec<String> = filter
        .split_whitespace()
        .map(|word| word.to_lowercase())
        .collect();
    rows.iter()
        .filter(|row| {
            let text = row.search_text();
            words.iter().all(|word| text.contains(word.as_str()))
        })
        .collect()
}
