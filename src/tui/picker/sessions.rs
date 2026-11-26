//! The session picker's rows: what one session's row says, which rows a
//! filter keeps, and the panel's rows. Pure, like
//! [`suggest`](super::super::suggest). [`view`](super::super::view) holds
//! whether the picker is open, which row is highlighted and which
//! directories it shows, and draws each row with the suggestion popup's
//! own row, so the two read as one kind of thing.

use std::path::Path;

use ratatui::text::{Line as UiLine, Span};

use crate::agent::session::io::{SessionFile, short_id};

use super::super::suggest::{Suggestion, suggestion_row, take_chars};
use super::super::theme::Palette;

/// The most of a name a row shows, when the row has the room.
pub(in crate::tui) const NAME_CAP: usize = 48;

/// The least of a name a row keeps when a project's path takes its room.
pub(in crate::tui) const NAME_FLOOR: usize = 12;

/// The most of a project's path a row keeps, however wide the row: the
/// end of it, because the end is what tells one worktree from another.
/// [`row_line`] keeps less of it on a narrow row.
pub(in crate::tui) const PROJECT_CAP: usize = 48;

/// Past this many days a row shows the date instead of an age.
const AGE_DAYS: i64 = 7;

/// One session as the picker shows it. The strings are built once, when
/// the picker opens or widens, so filtering and drawing never read the
/// disk or the clock.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// The session itself, for when its row is chosen.
    pub session: SessionFile,
    /// When the session started, relative to the moment the row was
    /// built: `just now`, `5m ago`, `3h ago`, `2d ago`, then the date.
    /// Empty when the file name carries no readable stamp.
    pub age: String,
    /// The 8-hex id from the file name: what `/resume <prefix>` takes.
    pub id: String,
    /// The title, else the first message's preview, else a stand-in, on
    /// one line and whole. [`row_line`] cuts it to the row.
    pub name: String,
    /// The directory the session started in, as the all-projects view
    /// shows it in the model's place. `None` in this project's view,
    /// where every row has the same directory.
    pub project: Option<String>,
    /// Lower-cased id, uuid, name, preview, model and the session's
    /// directory, in both views: what a filter word matches.
    haystack: String,
}

impl SessionRow {
    /// The row for `session` as this project's view shows it, aged
    /// against `now` (a Unix timestamp).
    pub fn new(session: SessionFile, now: i64) -> Self {
        Self::build(session, now, None)
    }

    /// The row as the all-projects view shows it: the session's
    /// directory, abbreviated against `home`, in the model's place.
    pub fn anywhere(session: SessionFile, now: i64, home: Option<&Path>) -> Self {
        let project = project_label(&session.cwd, home);
        Self::build(session, now, project)
    }

    fn build(session: SessionFile, now: i64, project: Option<String>) -> Self {
        let id = short_id(&session).to_string();
        let name = name_of(&session);
        let age = age(file_stamp(&session), now);
        let haystack = format!(
            "{} {} {name} {} {} {}",
            id,
            session.id,
            session.preview.as_deref().unwrap_or(""),
            session.model,
            session.cwd
        );
        Self {
            session,
            age,
            id,
            name,
            project,
            haystack: haystack.to_lowercase(),
        }
    }
}

/// An open picker: every project's rows and this project's, which of the
/// two it is showing, the highlighted row, the filter text the
/// highlight was last built for, and the last failed open's error. A
/// changed filter is a new list, so the highlight returns to the top row
/// (the rule the suggestion popup keeps in its own `suggest_filter`); a
/// scope change or a changed filter clears the error, which belongs to
/// the rows and the filter it was reported against.
#[derive(Debug, Clone)]
pub struct Picker {
    pub here: Vec<SessionRow>,
    pub everywhere: Vec<SessionRow>,
    pub all_directories: bool,
    pub selected: usize,
    pub filter: String,
    /// The last failed open's message, for the panel's own row: the
    /// panel covers the transcript, where the error is also kept for
    /// when the picker closes. `None` until one fails, and cleared by a
    /// scope change or a changed filter, which are a new list.
    pub error: Option<String>,
}

impl Picker {
    /// The picker over two listings of the same store, each newest first:
    /// this project's sessions (`here`) and every project's
    /// (`everywhere`, which includes the first). It opens showing every
    /// project's, each row aged against the clock now and its directory
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
        }
    }

    /// The rows in view, in listing order.
    pub fn rows(&self) -> &[SessionRow] {
        if self.all_directories {
            &self.everywhere
        } else {
            &self.here
        }
    }

    /// Whether the picker shows every project's sessions.
    pub fn all_directories(&self) -> bool {
        self.all_directories
    }

    /// Every project's sessions. The filter stays, and the highlight
    /// returns to the top row: it is a new list, and the last failed
    /// open belonged to the old one.
    pub fn widen(&mut self) {
        self.all_directories = true;
        self.selected = 0;
        self.error = None;
    }

    /// This project's sessions. The filter stays, the highlight returns
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

/// The rows `filter` keeps, in listing order: every whitespace-separated
/// word appears somewhere in the row (id, uuid, name, preview, model or
/// directory), matched case-insensitively. An empty filter keeps them
/// all.
pub fn matching<'a>(rows: &'a [SessionRow], filter: &str) -> Vec<&'a SessionRow> {
    let words: Vec<String> = filter
        .split_whitespace()
        .map(|word| word.to_lowercase())
        .collect();
    rows.iter()
        .filter(|row| {
            words
                .iter()
                .all(|word| row.haystack.contains(word.as_str()))
        })
        .collect()
}

/// How long before `now` (a Unix timestamp) the file-name stamp
/// `YYYYMMDD-HHMMSS` (UTC) is: `just now` under a minute, then minutes,
/// hours and days, and the date past a week. A stamp that is not in that
/// shape reads as nothing; one later than `now` (a clock that moved
/// back) as `just now`.
pub fn age(stamp: Option<&str>, now: i64) -> String {
    let Some([year, month, day, hour, minute, second]) = stamp.and_then(stamp_fields) else {
        return String::new();
    };
    // A relative age reads the same in every time zone; the stamps are
    // UTC, and so are the dates past a week.
    let started = epoch_of(year, month, day, hour, minute, second);
    let elapsed = u64::try_from(now - started).unwrap_or(0);
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let (unit, suffix) = if elapsed < MINUTE {
        return "just now".into();
    } else if elapsed < HOUR {
        (MINUTE, "m ago")
    } else if elapsed < DAY {
        (HOUR, "h ago")
    } else if elapsed < AGE_DAYS as u64 * DAY {
        (DAY, "d ago")
    } else {
        return format!("{year:04}-{month:02}-{day:02}");
    };
    format!("{}{suffix}", elapsed / unit)
}

/// The `YYYYMMDD-HHMMSS` a session file's name starts with, when it has
/// that shape (the stamp the listing sorts by, in the same stem as the
/// id [`short_id`](crate::agent::session::io::short_id) reads).
fn file_stamp(session: &SessionFile) -> Option<&str> {
    session
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(stamp, _)| stamp)
}

/// A UTC `YYYYMMDD-HHMMSS` stamp as its six fields, each checked against
/// what a calendar and a clock allow: year, month, day, hour, minute,
/// second.
fn stamp_fields(stamp: &str) -> Option<[i64; 6]> {
    if stamp.as_bytes().get(8) != Some(&b'-') {
        return None;
    }
    let mut fields = [0i64; 6];
    for (i, (from, to)) in [(0, 4), (4, 6), (6, 8), (9, 11), (11, 13), (13, 15)]
        .into_iter()
        .enumerate()
    {
        fields[i] = stamp.get(from..to)?.parse().ok()?;
    }
    let [year, month, day, hour, minute, second] = fields;
    let plausible = (1970..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..=23).contains(&hour)
        && (0..=59).contains(&minute)
        && (0..=59).contains(&second);
    plausible.then_some(fields)
}

/// Seconds since the Unix epoch of a UTC `YYYYMMDD-HHMMSS` stamp. Only
/// the tests call it: the calendar, checked against known instants.
#[cfg(test)]
pub(in crate::tui) fn stamp_epoch(stamp: &str) -> Option<i64> {
    let [year, month, day, hour, minute, second] = stamp_fields(stamp)?;
    Some(epoch_of(year, month, day, hour, minute, second))
}

/// Seconds since the Unix epoch of a UTC instant.
fn epoch_of(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// What a row calls the session: its title, else the first message's
/// preview, else a stand-in for a file with no messages. Whitespace runs
/// collapse, because a row is one line.
fn name_of(session: &SessionFile) -> String {
    let name = session
        .title
        .clone()
        .or_else(|| session.preview.clone())
        .unwrap_or_default();
    let flat = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "(no messages)".into();
    }
    flat
}

/// A session's directory as a row shows it: the home directory's prefix
/// becomes `~`, and what is still longer than [`PROJECT_CAP`] keeps its
/// end. `None` for a header with no directory.
pub fn project_label(cwd: &str, home: Option<&Path>) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let home = home
        .and_then(|home| home.to_str())
        .filter(|home| !home.is_empty());
    let under_home = home.and_then(|home| cwd.strip_prefix(home.trim_end_matches('/')));
    let label = match under_home {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => cwd.to_string(),
    };
    Some(tail(&label, PROJECT_CAP))
}

/// At most `max` chars of `text` from its start; when that cuts it, an
/// ellipsis is the last of them.
fn clip(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    format!(
        "{}…",
        text.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

/// At most `max` chars of `text` from its end; when that cuts it, an
/// ellipsis is the first of them. For a path, whose end says more than
/// its start.
pub(in crate::tui) fn tail(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!(
        "…{}",
        text.chars().skip(length - (max - 1)).collect::<String>()
    )
}

/// One row as the terminal sees it: the age, the id and the name as the
/// label; then dim to its right the model, or in the all-projects view
/// the session's directory in the model's place. The name is cut to
/// leave that its room, and the highlighted row is marked the way a
/// highlighted suggestion is (the suggestion popup's row draws it).
pub(in crate::tui) fn row_line(
    palette: &Palette,
    row: &SessionRow,
    selected: bool,
    width: usize,
) -> UiLine<'static> {
    let mut before = row.age.clone();
    if !before.is_empty() {
        before.push(' ');
    }
    before.push_str(&row.id);
    if !row.id.is_empty() {
        before.push_str("  ");
    }
    // What is left of the row, after the marker and the age and the id,
    // is the name's, a gap, and the description's.
    let room = width.saturating_sub(2 + before.chars().count());
    let (name_cap, description) = match &row.project {
        // A directory keeps its end, so the name gives ground first, down
        // to a floor; past that the directory keeps what end it can.
        Some(project) => {
            let project_room = project
                .chars()
                .count()
                .min(room.saturating_sub(NAME_FLOOR + 2));
            let name_cap = room
                .saturating_sub(2 + project_room)
                .clamp(NAME_FLOOR, NAME_CAP);
            (name_cap, tail(project, project_room))
        }
        // The model is secondary: it takes what the name leaves.
        None => (NAME_CAP, row.session.model.clone()),
    };
    let mut label = before;
    label.push_str(&clip(&row.name, name_cap));
    let suggestion = Suggestion {
        label,
        completion: String::new(),
        description,
    };
    suggestion_row(palette, &suggestion, selected, width)
}

/// The panel's one row when the picker is open but its scope holds no
/// row at all: the panel must not vanish over an empty scope, so it says
/// why in its own ground — the transcript, which the panel covers, has
/// nothing newer to say it in.
pub(in crate::tui) fn empty_row(
    palette: &Palette,
    scope_empty: bool,
    all_directories: bool,
    filter: &str,
    width: usize,
) -> UiLine<'static> {
    let text = if scope_empty && all_directories {
        "no sessions yet".to_string()
    } else if scope_empty {
        "no sessions in this directory".to_string()
    } else {
        format!("no session matches {:?}", filter.trim())
    };
    UiLine::from(Span::styled(
        take_chars(&format!(" {text}"), width),
        palette.popup_hint(),
    ))
}

/// The panel's last row after a failed open: the error itself, in the
/// warning's colour, in place of the hint — the panel covers the
/// transcript, where the error is also kept for when the picker closes.
pub(in crate::tui) fn error_row(palette: &Palette, error: &str, width: usize) -> UiLine<'static> {
    UiLine::from(Span::styled(
        take_chars(&format!("⚠ {error}"), width),
        palette.popup().fg(palette.warn),
    ))
}

/// The picker's last row: how to drive it, which scope Tab switches to,
/// and, when the list is longer than the panel, where the highlight
/// stands (`3/57`). The counter stands right after the move keys it
/// describes, not at the end: the keys and the scope survive a narrow
/// row, where the tail is what gets cut.
pub(in crate::tui) fn hint_row(
    palette: &Palette,
    width: usize,
    all_directories: bool,
    at: Option<(usize, usize)>,
) -> UiLine<'static> {
    let scope = if all_directories {
        "Tab: this directory"
    } else {
        "Tab: all directories"
    };
    let mut hint = String::from(" ↑↓ move");
    if let Some((selected, total)) = at {
        hint.push_str(&format!(" {}/{}", selected + 1, total));
    }
    hint.push_str(&format!(
        " · type filters · {scope} · Enter opens · Esc closes"
    ));
    UiLine::from(Span::styled(take_chars(&hint, width), palette.popup_hint()))
}
