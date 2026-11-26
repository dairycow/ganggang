//! The list's own code: what the composer's text opens ([`trigger`]),
//! one row of the list ([`Row`]), the open list ([`Picker`]), which rows
//! a filter keeps ([`matching`]), and the panel's own rows. A session's
//! row, and the rows a session listing builds, are
//! [`sessions`](self::sessions)'s.

use std::path::{Path, PathBuf};

use ratatui::text::{Line as UiLine, Span};

use super::suggest::{Suggestion, suggestion_row, take_chars};
use super::theme::Palette;
use crate::agent::session::io::SessionFile;

use self::sessions::{SessionRow, row_line};

pub(in crate::tui) mod sessions;

#[cfg(test)]
pub(in crate::tui) mod tests;

/// The kinds of list one panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Sessions,
}

/// The session listing a `/session` list is built from: the last scan's
/// result, the directory this run's sessions are filed in (where
/// `list_all_sessions`'s rows get their this-directory mark), and the
/// scan's bookkeeping. Built once, from `cwd`, when the state is made
/// and every time the run's directory changes.
#[derive(Debug, Clone)]
pub struct SessionListing {
    /// Every directory's sessions, newest first. Empty until a scan has
    /// returned.
    pub everywhere: Vec<SessionFile>,
    /// Where this directory's sessions are filed, `None` without HOME.
    pub dir: Option<PathBuf>,
    /// Home, as the every-directory rows abbreviate their directories
    /// against; read once, when the listing is made.
    pub home: Option<PathBuf>,
    /// A scan has returned.
    pub loaded: bool,
    /// A scan is in flight.
    pub scanning: bool,
    /// A scan was requested.
    pub wanted: bool,
    /// The previous scan's warnings: the next scan says only what changed.
    pub warnings: Vec<String>,
}

impl SessionListing {
    /// The listing before any scan has run: empty, no warnings, its
    /// directory filed from `cwd`.
    pub fn new(cwd: &Path) -> Self {
        Self {
            everywhere: Vec::new(),
            dir: crate::agent::session::io::sessions_dir(cwd),
            home: crate::home::dir(),
            loaded: false,
            scanning: false,
            wanted: false,
            warnings: Vec::new(),
        }
    }
}

/// What the composer's text opens: which list, with which filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub list: List,
    pub filter: String,
}

/// `text`'s first word is `/session`: the list it opens, with the rest of
/// the line, trimmed, as its filter. Nothing else is a trigger: `/sessionx`
/// and a plain prompt open nothing.
pub(in crate::tui) fn trigger(text: &str) -> Option<Trigger> {
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    (head == "/session").then(|| Trigger {
        list: List::Sessions,
        filter: rest.trim().to_string(),
    })
}

/// The one-line actions the session list's rows act on, beside the
/// sessions themselves: the copy runs only when a filter is typed, which
/// is what `/session copy` and `/session info` submit as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Copy the current session's `.jsonl` path to the clipboard.
    CopyPath,
    /// Show the current session's path, model, entries and tokens.
    Info,
}

impl Action {
    /// The action's row text: its label.
    fn label(&self) -> &'static str {
        match self {
            Action::CopyPath => "copy path",
            Action::Info => "info",
        }
    }
}

/// One action row of the session list, as the popup's row builders draw
/// it: the label, the description that says what it does.
pub fn action_row(action: Action) -> Suggestion {
    match action {
        Action::CopyPath => Suggestion {
            label: "copy path".to_string(),
            completion: String::new(),
            description: "copy this session's .jsonl path to the clipboard".to_string(),
        },
        Action::Info => Suggestion {
            label: "info".to_string(),
            completion: String::new(),
            description: "session path, model, entries, token totals".to_string(),
        },
    }
}

/// One row of the list.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Row {
    /// A session's row, as the panel draws it.
    Session(SessionRow),
    /// An action row, beside the sessions: it shows above them while a
    /// filter is typed, and its search text is its label.
    Action(Action),
}

impl Row {
    /// Lower-cased text a filter word has to appear in.
    pub fn search_text(&self) -> &str {
        match self {
            Row::Session(row) => &row.haystack,
            Row::Action(action) => action.label(),
        }
    }

    /// The row as the panel draws it.
    pub fn line(&self, palette: &Palette, selected: bool, width: usize) -> UiLine<'static> {
        match self {
            Row::Session(row) => row_line(palette, row, selected, width),
            Row::Action(action) => suggestion_row(palette, &action_row(*action), selected, width),
        }
    }

    /// Whether both rows are the same thing: what keeps the highlight
    /// across a refresh.
    pub fn same_row(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Session(a), Row::Session(b)) => a.session.path == b.session.path,
            (Row::Action(a), Row::Action(b)) => a == b,
            _ => false,
        }
    }

    /// The session the row was built from; `None` on an action row.
    #[allow(dead_code)]
    pub fn session(&self) -> Option<&SessionFile> {
        match self {
            Row::Session(row) => Some(&row.session),
            Row::Action(_) => None,
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
    /// This directory's sessions, newest first.
    here: Vec<SessionRow>,
    /// Every directory's sessions, newest first, this directory's
    /// included.
    everywhere: Vec<SessionRow>,
    /// Whether the list shows every directory's sessions.
    all_directories: bool,
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
        }
    }

    /// The session rows in view, in listing order.
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

/// The panel's one row when the list is open but has nothing to show:
/// `reason` says why, in the panel's own ground — the transcript, which
/// the panel covers, has nothing newer to say it in.
pub(in crate::tui) fn empty_row(palette: &Palette, reason: &str, width: usize) -> UiLine<'static> {
    UiLine::from(Span::styled(
        take_chars(&format!(" {reason}"), width),
        palette.popup_hint(),
    ))
}

/// The panel's last row after a failed action: the error itself, in the
/// warning's colour, in place of the hint — the panel covers the
/// transcript, where the error is also kept for when the list closes.
pub(in crate::tui) fn error_row(palette: &Palette, error: &str, width: usize) -> UiLine<'static> {
    UiLine::from(Span::styled(
        take_chars(&format!("⚠ {error}"), width),
        palette.popup().fg(palette.warn),
    ))
}

/// The panel's last row: how to drive the list, and where the highlight
/// stands (`at` is `Some((selected, total))` only when the list is
/// longer than the panel). `scope`: `Some(true)` when every directory's
/// sessions show (the hint offers this directory's), `Some(false)` for
/// this directory's (it offers every directory's), `None` for a list with
/// no scope key. The counter stands right after the move keys it
/// describes, not at the end: the keys and the scope survive a narrow
/// row, where the tail is what gets cut.
pub(in crate::tui) fn hint_row(
    palette: &Palette,
    width: usize,
    scope: Option<bool>,
    at: Option<(usize, usize)>,
) -> UiLine<'static> {
    let scope = match scope {
        Some(true) => "Tab: this directory",
        Some(false) => "Tab: all directories",
        None => "Tab: complete",
    };
    let mut hint = String::from(" ↑↓ move");
    if let Some((selected, total)) = at {
        hint.push_str(&format!(" {}/{}", selected + 1, total));
    }
    hint.push_str(&format!(
        " · type filters · {scope} · Enter picks · Esc clears"
    ));
    UiLine::from(Span::styled(take_chars(&hint, width), palette.popup_hint()))
}
