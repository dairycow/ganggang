//! The run's half of [`State`](super::super::State): what a run starting and ending does to the
//! view, what one Ctrl+C means, prompt recall, and the transcript's scroll
//! position. The struct, the transcript accessors, and the footer plumbing
//! are in [`state`](super::super::state).

use crate::tui::history::{Arrow, history_for};

impl crate::tui::view::State {
    pub fn run_finished(&mut self) {
        self.busy = false;
        self.interrupt_armed = false;
        self.footer.set_busy(false);
        self.footer.set_stopping(false);
    }

    /// Whether the footer is showing the stopping mark. The footer is its
    /// only home: it keeps the mark tied to its own busy flag, so it can
    /// never outlive a run.
    pub fn stopping(&self) -> bool {
        self.footer.stopping()
    }

    /// The stopping mark: set only while a run is live, which the footer
    /// enforces (an idle view's mark is ignored, so Esc on an idle view
    /// leaves the footer alone).
    pub fn set_stopping(&mut self, stopping: bool) {
        self.footer.set_stopping(stopping);
    }

    /// What one Ctrl+C does (gap §5), in one place so the three cases are
    /// testable without a terminal: cancel a live run, clear a draft, or —
    /// when there is nothing left to cancel — end the TUI. The caller (the
    /// event loop) sets the abort flag and quits, because those are the two
    /// things the view cannot do.
    pub fn interrupt(&mut self) -> Cancel {
        if self.busy {
            // A run Ctrl+C has already asked to stop: this press is the
            // confirmation. (An Esc that stopped the run does not arm it.)
            let forced = self.interrupt_armed;
            self.interrupt_armed = true;
            self.set_stopping(true);
            return if forced {
                Cancel::ForceQuit
            } else {
                Cancel::Stopping
            };
        }
        if self.editor.text().is_empty() {
            return Cancel::Quit;
        }
        self.editor.clear();
        // A browse's draft goes with the text it displaced.
        self.history.reset();
        Cancel::DraftCleared
    }

    /// ↑ / Ctrl+P: the previous prompt. The text browsing displaces is
    /// saved as the browse draft, so a later ↓ gives it back; the composer
    /// takes the recalled text and stays editable.
    pub fn recall_prev(&mut self) {
        let current = self.editor.text().to_string();
        if let Some(recalled) = self.history.prev(&current) {
            self.editor.set_text(&recalled);
        }
    }

    /// ↓ / Ctrl+N: the next prompt, and past the newest the draft that was
    /// in the composer when browsing began.
    pub fn recall_next(&mut self) {
        if let Some(next) = self.history.next() {
            self.editor.set_text(&next);
        }
    }

    /// What one arrow key means for this composer: the popup while it is
    /// open, then history, then the composer's own lines.
    pub fn arrow(&self, up: bool) -> Arrow {
        history_for(
            up,
            self.suggest_open(),
            self.history.browsing(),
            self.editor.line_count(),
        )
    }

    pub fn scroll_up(&mut self, by: u16) {
        // Rows, not logical lines: wrapping only adds rows, so the line
        // count is the pre-render lower bound and the render pass
        // publishes the exact figure each frame.
        let rows = self
            .last_row_count
            .max(self.transcript.render().len() as u16);
        // The far end is the first row reaching the top of the view — one
        // row short of the count, the old rule, until a render has said
        // how tall the view is. Past it the text would sit still while
        // the offset kept growing, and leave a gap above the composer.
        let view = self.last_view_rows.max(1);
        let max = rows.saturating_sub(view);
        self.offset_from_bottom = (self.offset_from_bottom + by).min(max);
    }

    pub fn scroll_down(&mut self, by: u16) {
        self.offset_from_bottom = self.offset_from_bottom.saturating_sub(by);
    }
}

/// What one Ctrl+C did, and what the event loop owes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancel {
    /// A live run was asked to stop: set the abort flag, and say so.
    Stopping,
    /// A second Ctrl+C on a run already stopping: set the flag, then end.
    ForceQuit,
    /// Idle: the composer's text is gone, and the next Ctrl+C quits.
    DraftCleared,
    /// Idle with nothing typed: end.
    Quit,
}

impl crate::tui::view::State {
    #[cfg(test)]
    pub(crate) fn offset_for_test(&self) -> u16 {
        self.offset_from_bottom
    }
}
