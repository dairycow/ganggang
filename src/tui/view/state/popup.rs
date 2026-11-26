//! The suggestion popup's half of [`State`](super::super::State): which row is highlighted, and
//! the rule that a highlight belongs to the composer text it was built
//! from. The rows themselves come from
//! [`suggest`](crate::tui::suggest); this is only the selection,
//! kept next to the popup that reads it.

impl crate::tui::view::State {
    /// The live suggestion list for what the composer holds.
    pub fn suggestions(&self) -> Vec<crate::tui::suggest::Suggestion> {
        crate::tui::suggest::suggestions(
            self.editor.text(),
            &self.skills,
            &self.models,
            &self.active_model,
        )
    }

    /// Whether the popup should draw: a bare command word that matches
    /// at least one row. The `/resume` picker has the composer while it
    /// is open, so a filter that happens to start with `/` is a filter.
    pub fn suggest_open(&self) -> bool {
        self.resume.is_none()
            && crate::tui::suggest::active(self.editor.text())
            && !self.suggestions().is_empty()
    }

    /// Whether a popup has the arrows: the suggestion list or the
    /// `/resume` picker.
    pub fn popup_open(&self) -> bool {
        self.resume.is_some() || self.suggest_open()
    }

    /// ↑ while a popup is open: the picker's highlight when the `/resume`
    /// picker is open, the suggestion list's otherwise.
    pub fn popup_prev(&mut self) {
        if self.resume.is_some() {
            self.resume_prev();
        } else {
            self.suggest_prev();
        }
    }

    /// ↓ while a popup is open: the other direction of
    /// [`popup_prev`](Self::popup_prev).
    pub fn popup_next(&mut self) {
        if self.resume.is_some() {
            self.resume_next();
        } else {
            self.suggest_next();
        }
    }

    /// ↑: the highlight climbs, stopping at the top.
    pub fn suggest_prev(&mut self) {
        self.sync_suggest();
        self.suggest_selected = self.suggest_selected.saturating_sub(1);
    }

    /// ↓: the highlight descends, stopping at the last VISIBLE row —
    /// a short terminal draws fewer rows than the list holds, and an
    /// off-screen highlight is a trap (Tab completes what nobody can
    /// see). The popup does not scroll; narrowing the prefix does.
    pub fn suggest_next(&mut self) {
        self.sync_suggest();
        let last = self.last_selectable_row();
        self.suggest_selected = (self.suggest_selected + 1).min(last);
    }

    /// Tab: the highlighted row's completion replaces the composer.
    pub fn complete_suggestion(&mut self) {
        self.sync_suggest();
        let list = self.suggestions();
        let last = self.last_selectable_row();
        if let Some(suggestion) = list.get(self.suggest_selected.min(last)) {
            self.editor.set_text(&suggestion.completion);
        }
    }

    /// The highest row index the popup currently lets the selection
    /// reach: the last drawn row when render has published one, else
    /// the last row of the whole list.
    fn last_selectable_row(&self) -> usize {
        let len = self.suggestions().len();
        let rows = if self.suggest_view_rows == 0 {
            len
        } else {
            self.suggest_view_rows.min(len)
        };
        rows.saturating_sub(1)
    }

    /// Reset the highlight whenever the composer text changed — the
    /// list is a pure function of the text, so a changed text is a new
    /// list and the top row is the honest default again. render() calls
    /// this every frame, and so does every popup key handler: the
    /// selection is never allowed to point into a stale list.
    pub(crate) fn sync_suggest(&mut self) {
        if self.editor.text() != self.suggest_filter {
            self.suggest_filter = self.editor.text().to_string();
            self.suggest_selected = 0;
        }
    }
}

impl crate::tui::view::State {
    /// Park the popup's drawn-row count, as render would publish it —
    /// short-terminal clamping without a terminal.
    #[cfg(test)]
    pub(crate) fn set_suggest_view_rows_for_test(&mut self, rows: usize) {
        self.suggest_view_rows = rows;
    }
}
