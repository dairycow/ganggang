//! The list's own code: what the composer's text opens ([`trigger`]),
//! the rows a list is built from (an action, a model, a skill), and the
//! panel's own rows. One row of the list, the open list and the filter
//! are [`list`](self::list)'s; a session's row, and the rows a session
//! listing builds, are [`sessions`](self::sessions)'s.

use std::path::{Path, PathBuf};

use ratatui::text::{Line as UiLine, Span};

use super::models::ModelChoice;
use super::suggest::{Suggestion, take_chars};
use super::theme::Palette;
use crate::agent::session::io::SessionFile;
use crate::agent::skills::Skill;

pub(in crate::tui) mod list;
pub(in crate::tui) mod sessions;

#[cfg(test)]
pub(in crate::tui) mod tests;

pub(in crate::tui) use list::{Pick, Picker, Row, matching};
#[cfg(test)]
pub(in crate::tui) use sessions::SessionRow;

/// The kinds of list one panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Sessions,
    Models,
    Skills,
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
    /// The arguments typed after the filter word: `/skill` only, empty
    /// otherwise.
    pub args: String,
}

/// `text`'s first word is `/session`, `/model` or `/skill`: the list it
/// opens, with the rest of the line, trimmed, as its filter. For
/// `/skill` the filter is the first word of the rest and the remainder
/// the arguments the picked skill runs with. Nothing else is a trigger:
/// `/sessionx`, `/modelx`, `/skillx` and a plain prompt open nothing.
pub(in crate::tui) fn trigger(text: &str) -> Option<Trigger> {
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    match head {
        "/session" => Some(Trigger {
            list: List::Sessions,
            filter: rest.trim().to_string(),
            args: String::new(),
        }),
        "/model" => Some(Trigger {
            list: List::Models,
            filter: rest.trim().to_string(),
            args: String::new(),
        }),
        "/skill" => {
            let mut words = rest.trim_start().splitn(2, char::is_whitespace);
            Some(Trigger {
                list: List::Skills,
                filter: words.next().unwrap_or("").to_string(),
                args: words.next().unwrap_or("").trim().to_string(),
            })
        }
        _ => None,
    }
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
    pub(super) fn label(&self) -> &'static str {
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

/// One model row of the `/model` list, as the popup's row builders draw
/// it: the `provider/model` label, a `/model provider/model` completion
/// (no trailing space: the argument IS the completion), and the context
/// window, discovery status, key state and active mark in the
/// description.
pub fn model_row(m: &ModelChoice, active_label: &str) -> Suggestion {
    let label = format!("{}/{}", m.provider, m.model);
    let mut description = format!("ctx {}k", m.context_window / 1000);
    if let Some(status) = &m.status {
        description.push_str(&format!(" · {status}"));
    }
    if m.needs_key && !m.has_key {
        description.push_str(" · no key");
    }
    if label == active_label {
        description.push_str(" · active");
    }
    Suggestion {
        label,
        completion: format!("/model {}/{}", m.provider, m.model),
        description,
    }
}

/// One skill row of the `/skill` list, as the popup's row builders draw
/// it: the name, the description with a `· manual only` mark when the
/// skill is hidden from the model's own system prompt, and a
/// `/skill <name> ` completion (a trailing space: the arguments are still
/// to come).
pub fn skill_row(skill: &Skill) -> Suggestion {
    let mut description = skill.description.clone();
    if skill.disable_model_invocation {
        description.push_str(" · manual only");
    }
    Suggestion {
        label: skill.name.clone(),
        completion: format!("/skill {} ", skill.name),
        description,
    }
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

/// What the hint row says of the list's own keys, after "type filters":
/// the session list's `Tab` (which scope it switches to) and its `Enter`,
/// or the `Enter` then the `Tab` of a list whose `Tab` completes the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum HintKeys {
    Sessions { every_directory: bool },
    Complete { enter: &'static str },
}

/// The panel's last row: how to drive the list, and where the highlight
/// stands (`at` is `Some((selected, total))` only when the list is longer
/// than the panel). The counter stands right after the move keys it
/// describes, not at the end: the keys survive a narrow row, where the
/// tail is what gets cut.
pub(in crate::tui) fn hint_row(
    palette: &Palette,
    width: usize,
    keys: HintKeys,
    at: Option<(usize, usize)>,
) -> UiLine<'static> {
    let own = match keys {
        HintKeys::Sessions { every_directory } => {
            let scope = if every_directory {
                "this directory"
            } else {
                "all directories"
            };
            format!("Tab: {scope} · Enter picks")
        }
        HintKeys::Complete { enter } => format!("{enter} · Tab completes"),
    };
    let mut hint = String::from(" ↑↓ move");
    if let Some((selected, total)) = at {
        hint.push_str(&format!(" {}/{}", selected + 1, total));
    }
    hint.push_str(&format!(" · type filters · {own} · Esc clears"));
    UiLine::from(Span::styled(take_chars(&hint, width), palette.popup_hint()))
}
