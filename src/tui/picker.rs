//! The session picker module. What the composer's text opens
//! ([`trigger`]), the session rows and their filter in
//! [`sessions`](self::sessions), and the panel's own rows as the picker
//! generalises.

pub(in crate::tui) mod sessions;

#[cfg(test)]
pub(in crate::tui) mod tests;

pub(in crate::tui) use sessions::*;

/// The kinds of list one panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Sessions,
}

/// The session listing a `/resume` list is built from: the last scan's
/// result, the directory this run's sessions are filed in (where
/// `list_all_sessions`'s rows get their this-directory mark), and the
/// scan's bookkeeping. Built once, from `cwd`, when the state is made
/// and every time the run's directory changes.
#[derive(Debug, Clone)]
pub struct SessionListing {
    /// Every directory's sessions, newest first. Empty until a scan has
    /// returned.
    pub everywhere: Vec<crate::agent::session::io::SessionFile>,
    /// Where this directory's sessions are filed, `None` without HOME.
    pub dir: Option<std::path::PathBuf>,
    /// Home, as the every-directory rows abbreviate their directories
    /// against; read once, when the listing is made.
    pub home: Option<std::path::PathBuf>,
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
    pub fn new(cwd: &std::path::Path) -> Self {
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

/// `text`'s first word is `/resume`: the list it opens, with the rest of
/// the line, trimmed, as its filter. Nothing else is a trigger: `/resum`,
/// `/resumex`, `/resume:x` and a plain prompt open nothing.
pub(in crate::tui) fn trigger(text: &str) -> Option<Trigger> {
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    (head == "/resume").then(|| Trigger {
        list: List::Sessions,
        filter: rest.trim().to_string(),
    })
}

#[cfg(test)]
mod trigger_tests {
    use super::*;

    /// The trigger, as `(list, filter)` where one is.
    fn of(text: &str) -> Option<(List, String)> {
        trigger(text).map(|t| (t.list, t.filter))
    }

    #[test]
    fn the_resume_command_opens_the_session_list_with_the_rest_as_its_filter() {
        assert_eq!(
            of("/resume"),
            Some((List::Sessions, String::new())),
            "the bare command: an empty filter"
        );
        assert_eq!(
            of("/resume glm fix"),
            Some((List::Sessions, "glm fix".into())),
            "every word after the command is the filter"
        );
        assert_eq!(
            of("/resume   "),
            Some((List::Sessions, String::new())),
            "trailing whitespace is no filter"
        );
        for text in [
            "/resum",
            "/resumex",
            "/resume:x",
            "resume",
            "hello there",
            "/model",
        ] {
            assert_eq!(of(text), None, "{text:?} opens nothing");
        }
        assert_eq!(of(""), None, "an empty composer opens nothing");
    }
}
