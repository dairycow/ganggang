//! Prompt history (gap §4): what has been submitted, and where a browse
//! stands in it.
//!
//! Two shapes share the module: [`History`] (the remembered texts, newest
//! last, with the browse in progress) and [`file`], the persistence half —
//! `~/.gg/history.jsonl`, one JSON string per line. The composer-level
//! rule, when an arrow key means history at all, is [`history_for`]:
//! pure, so it is testable without a terminal.

mod file;

use std::path::PathBuf;

/// Entries kept, in memory and on disk: the newest this many.
const MAX_ENTRIES: usize = 500;

/// What has been submitted, newest last, plus the browse in progress.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    /// The file's line count, tracked rather than re-read: it decides
    /// whether the next write is an append or a rewrite.
    lines: usize,
    path: Option<PathBuf>,
    browse: Option<Browse>,
    /// Lines dropped while loading, for one warning rather than many.
    skipped: usize,
}

/// One browsing session: the composer text browsing displaced, and the
/// index of the entry the composer now shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Browse {
    draft: String,
    index: usize,
}

impl History {
    /// Read the persisted history (`~/.gg/history.jsonl`, or nowhere
    /// without HOME, where a history is in-memory only). A missing or
    /// unreadable file is simply no history: history is a convenience and
    /// must never decide whether the TUI starts.
    pub fn load() -> Self {
        let path = file::path();
        let (entries, skipped) = match &path {
            Some(path) => file::read(path, MAX_ENTRIES),
            None => (Vec::new(), 0),
        };
        let lines = match &path {
            Some(path) => file::count_lines(path),
            None => 0,
        };
        History {
            entries,
            lines,
            path,
            browse: None,
            skipped,
        }
    }

    /// One submitted text: remembered, newest last. Surrounding
    /// whitespace goes (interior newlines stay — a multi-line prompt is
    /// one prompt). Trimmed-empty text is not a submission, and neither is
    /// an exact repeat of the entry already at the end: one repeated
    /// follow-up is one entry, so walking back does not stop twice on it.
    pub fn record(&mut self, text: &str) {
        // A submission ends a browse whatever else it does: the recalled
        // text has been sent, so the next ↑ starts from the newest again.
        self.browse = None;
        let text = text.trim();
        if text.is_empty() || self.entries.last().is_some_and(|last| last == text) {
            return;
        }
        self.push(text.to_string());
        self.persist_new(text);
    }

    /// ↑: the first press parks the composer's current text as the browse
    /// draft and shows the newest entry; later presses walk older, and the
    /// oldest is where they stop.
    pub fn prev(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        match &mut self.browse {
            None => {
                self.browse = Some(Browse {
                    draft: current.to_string(),
                    index: self.entries.len() - 1,
                });
            }
            Some(browse) if browse.index > 0 => browse.index -= 1,
            // Already at the oldest: nothing older to show, and a composer
            // the user has since edited is not written over.
            Some(_) => return None,
        }
        Some(self.entries[self.browse.as_ref()?.index].clone())
    }

    /// ↓: newer, and past the newest the saved draft comes back and the
    /// browsing ends — the composer is the user's again.
    pub fn next(&mut self) -> Option<String> {
        let len = self.entries.len();
        let index = self.browse.as_ref()?.index;
        if index + 1 < len {
            self.browse.as_mut()?.index = index + 1;
            return Some(self.entries[index + 1].clone());
        }
        Some(self.browse.take()?.draft)
    }

    /// Browsing is over: the next ↑ starts from the newest entry again.
    /// Called when a submission goes out or a new session starts.
    pub fn reset(&mut self) {
        self.browse = None;
    }

    /// Whether the composer is showing a recalled entry.
    pub fn browsing(&self) -> bool {
        self.browse.is_some()
    }

    /// What the browse session displaced, if it is still displaced: the
    /// draft a later ↑/↓ swaps in or out of the composer. The composer
    /// itself is what holds it in between, so only the tests ask.
    #[cfg(test)]
    pub fn draft(&self) -> Option<&str> {
        self.browse.as_ref().map(|browse| browse.draft.as_str())
    }

    /// One warning when a load skipped lines. A `history.jsonl` edited by
    /// hand or torn by a crash is the user's to notice, not gg's to fix.
    pub fn warning(&self) -> Option<String> {
        if self.skipped == 0 {
            return None;
        }
        Some(format!(
            "{} line(s) in {} were not prompt history and were skipped",
            self.skipped,
            self.path.as_ref()?.display()
        ))
    }

    fn push(&mut self, entry: String) {
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// One entry, one appended line; past [`file::COMPACT_AFTER`] lines the
    /// file is rewritten instead (a rewrite writes the whole cap, so it is
    /// not counted as an append). Best effort throughout: a history that
    /// cannot be written is still a history in memory.
    fn persist_new(&mut self, text: &str) {
        let Some(path) = &self.path else { return };
        if self.lines > file::COMPACT_AFTER {
            if file::compact(path, &self.entries) {
                self.lines = self.entries.len();
            }
            return;
        }
        if file::append(path, text) {
            self.lines += 1;
        }
    }

    /// The entries, oldest first.
    #[cfg(test)]
    fn entries(&self) -> &[String] {
        &self.entries
    }

    /// History at an explicit path — the file's test seam; otherwise the
    /// same load [`load`](Self::load) does. A path with nothing readable at
    /// it is history that never touches the disk.
    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        let (entries, skipped) = file::read(&path, MAX_ENTRIES);
        let lines = file::count_lines(&path);
        History {
            entries,
            lines,
            path: Some(path),
            browse: None,
            skipped,
        }
    }
}

/// Which editing action one arrow press means (gap §4's rule), decided
/// before any state moves:
///
/// * a browse in progress owns the arrows — ↑ older, ↓ newer, whatever the
///   recalled text looks like: a multi-line prompt must stay walkable, and
///   the suggestion list or `/session`, `/model`, `/skill` list a recalled
///   command opens over the composer does not take them, or the newest
///   entry would be as far as a walk gets;
/// * the suggestion popup owns them next, while it is open;
/// * ↑ starts a browse from an empty composer or a single logical line;
/// * a multi-line composer keeps its line-by-line cursor, and ↓ there —
///   with no browse running, it has no history to walk — does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    /// ↑: the popup's previous row.
    SuggestPrev,
    /// ↓: the popup's next row.
    SuggestNext,
    /// ↑: an older prompt.
    Prev,
    /// ↓: a newer prompt, or the draft a browse displaced.
    Next,
    /// ↑: the cursor, one line up.
    LineUp,
    /// ↓: the cursor, one line down.
    LineDown,
    /// Nothing: ↓ with no browse to walk.
    Nothing,
}

/// `logical_lines`: the composer's lines, so 1 for ordinary typing and 0
/// for a composer holding nothing at all (see
/// [`line_count`](super::input::Editor::line_count)).
pub fn history_for(up: bool, popup_open: bool, browsing: bool, logical_lines: usize) -> Arrow {
    if browsing {
        return if up { Arrow::Prev } else { Arrow::Next };
    }
    if popup_open {
        return if up {
            Arrow::SuggestPrev
        } else {
            Arrow::SuggestNext
        };
    }
    if logical_lines > 1 {
        return if up { Arrow::LineUp } else { Arrow::LineDown };
    }
    if up { Arrow::Prev } else { Arrow::Nothing }
}

#[cfg(test)]
mod tests;
