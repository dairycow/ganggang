//! The view's state: what the loop, the commands, and the agent's events
//! change between frames — the transcript, the composer, the footer, the
//! scroll offset, the suggestion popup's selection, and the palette picked
//! once at startup. No drawing here and no colour: the geometry is
//! [`layout`](super::super::layout)'s, the style is
//! [`theme`](super::super::theme)'s, and what a line means is the
//! transcript model's.
//!
//! The struct and its construction live here; its behaviour is split by
//! what it is about: [`log`] is the conversation (what is pushed, seeded,
//! and measured), [`turn`] the live run (starting, stopping, recalling,
//! scrolling), [`queue`] the prompts held back while a run works,
//! [`popup`] the suggestion list's selection and which popup
//! has the arrows, [`picker`] a list (what is open, its highlight, its
//! keys), and [`sessions`] the session listing a `/session` list is built
//! from.

mod log;
mod picker;
mod popup;
mod queue;
mod sessions;
mod turn;

pub use turn::Cancel;

use std::sync::Arc;

use super::super::footer::Footer;
use super::super::history::History;
use super::super::input::Editor;
use super::super::theme::Palette;
use super::super::transcript::Transcript;

/// Everything the draw closure needs; owned by the event loop.
pub struct State {
    pub editor: Editor,
    pub footer: Footer,
    pub busy: bool,
    /// What has been submitted this session and before it, and where a
    /// recall stands (gap §4). Loaded once, by `run`.
    pub history: History,
    /// A Ctrl+C has already been spent on the live run: the next one while
    /// it is still busy ends the TUI (gap §5). The stopping mark the footer
    /// shows is a separate thing — Esc marks a run stopping too, and an Esc
    /// must not turn the next Ctrl+C into an exit nobody asked for. Cleared
    /// by any run ending and by any new run starting.
    interrupt_armed: bool,
    pub(super) transcript: Transcript,
    /// Transcript scroll, measured from the BOTTOM: 0 pins to the newest
    /// row (auto-follow); paging up grows it. New content arriving keeps
    /// the view anchored wherever the user left it.
    pub(super) offset_from_bottom: u16,
    /// Wrapped-row count from the last render — the clamp for scrolling.
    pub(super) last_row_count: u16,
    /// Rows the transcript showed on the last render: with the row count,
    /// the far end of the scroll clamp. 0 = not drawn yet.
    pub(super) last_view_rows: u16,
    /// What `/session` reports; `None` path = not persisted.
    pub session_path: Option<std::path::PathBuf>,
    pub sessions_disabled: bool,
    /// Totals from the last run (§13: summed across the session).
    pub entries: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The session listing the `/session` list is built from: the last
    /// scan's result, where this directory's sessions are filed, and the
    /// scan's bookkeeping. The tests sit in the module tree and reach in
    /// through it.
    pub sessions: crate::tui::picker::SessionListing,
    /// Prompts Enter held back while a run worked, and whether a stop or
    /// a failure is holding them. The behaviour is [`queue`]'s.
    queue: queue::Queue,
    /// The run in flight says it was stopped: `RunAborted` sets this, and
    /// the run's outcome reads and clears it (in `run_finished`). A flag
    /// rather than a read of the transcript, so a block that lands between
    /// the abort mark and the outcome cannot hide the stop.
    run_stopped: bool,
    /// The directory the session runs in: where `@mentions` in a queued
    /// prompt resolve when its turn comes (they expand at send time, as
    /// they do for a prompt typed at rest).
    cwd: std::path::PathBuf,
    /// The `/session` list while it is open; the text after the command
    /// is its filter. `None` = closed.
    pub picker: Option<crate::tui::picker::Picker>,
    /// Configured context window — the denominator of the footer's %.
    context_window: u64,
    last_context_tokens: u64,
    /// The discovery sets the suggestion popup lists from (§15
    /// amendment): the rows of the `/skill` list. Shared read-only
    /// with the command handler.
    skills: Arc<Vec<crate::agent::skills::Skill>>,
    /// The model rows the `/model` popup lists from, and the active
    /// model's `provider/model` label, so its row reads `· active`.
    /// Shared read-only with the command handler, like `skills`.
    models: Arc<Vec<crate::tui::models::ModelChoice>>,
    active_model: String,
    /// Highlighted row of the suggestion popup. Resets to the top
    /// whenever the composer text changes (render syncs it); ↑/↓ move
    /// it, Tab completes from it.
    pub(super) suggest_selected: usize,
    /// The composer text the selection was last synced against.
    suggest_filter: String,
    /// Rows the popup last actually DREW (render publishes it, like
    /// `last_row_count`): a short terminal shows fewer rows than the
    /// list holds, and the selection must never point past what is on
    /// screen — Tab would complete a row the user cannot see. 0 = not
    /// drawn yet (popup keys clamp to the full list, as tests do).
    pub(super) suggest_view_rows: usize,
    /// The palette this session draws with, chosen once at startup.
    pub(super) palette: Palette,
}

impl State {
    pub fn new(
        cwd: &std::path::Path,
        model: &str,
        context_window: u32,
        skills: Arc<Vec<crate::agent::skills::Skill>>,
        history: History,
    ) -> Self {
        Self::with_palette(
            cwd,
            model,
            context_window,
            skills,
            history,
            super::super::theme::palette_from_env(),
        )
    }

    /// The same, with the palette supplied: what a test of the drawing
    /// wants, and what `run` uses once it has read the environment.
    pub fn with_palette(
        cwd: &std::path::Path,
        model: &str,
        context_window: u32,
        skills: Arc<Vec<crate::agent::skills::Skill>>,
        history: History,
        palette: Palette,
    ) -> Self {
        Self {
            editor: Editor::default(),
            footer: Footer::new(cwd, model),
            busy: false,
            history,
            interrupt_armed: false,
            transcript: Transcript::default(),
            offset_from_bottom: 0,
            last_row_count: 0,
            last_view_rows: 0,
            session_path: None,
            sessions_disabled: false,
            entries: 0,
            tokens_in: 0,
            tokens_out: 0,
            sessions: crate::tui::picker::SessionListing::new(cwd),
            picker: None,
            queue: queue::Queue::default(),
            run_stopped: false,
            cwd: cwd.to_path_buf(),
            context_window: context_window as u64,
            last_context_tokens: 0,
            skills,
            models: Arc::new(Vec::new()),
            active_model: model.to_string(),
            suggest_selected: 0,
            suggest_filter: String::new(),
            suggest_view_rows: 0,
            palette,
        }
    }

    /// The palette in use.
    pub fn palette(&self) -> Palette {
        self.palette
    }

    /// The directory the session runs in.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// The model rows the `/model` list is built from, as an `Arc`: the
    /// command handler reads the set while a `/reload` cannot swap it out.
    pub fn models_arc(&self) -> Arc<Vec<crate::tui::models::ModelChoice>> {
        self.models.clone()
    }
    /// Test seam: the active model's label, what `set_model` would set
    /// it to.
    #[cfg(test)]
    pub(crate) fn set_active_model_for_test(&mut self, label: &str) {
        self.active_model = label.to_string();
    }

    /// The shared skill set, as an `Arc`: what a `/reload` replaces and
    /// what the command handler snapshots for its lists.
    pub fn skills_arc(&self) -> Arc<Vec<crate::agent::skills::Skill>> {
        self.skills.clone()
    }

    /// Replace the skill set (a `/reload` or a swapped agent). While the
    /// `/skill` list is open the rows are its new rows: the user's filter
    /// is kept, and the highlight stays on the skill it stood on, or
    /// returns to the list's default when that skill is gone. The
    /// suggestion popup lists built-ins only, so nothing of its is
    /// touched.
    pub fn set_skills(&mut self, skills: Arc<Vec<crate::agent::skills::Skill>>) {
        self.skills = skills;
        if self
            .picker
            .as_ref()
            .is_some_and(|open| open.list == crate::tui::picker::List::Skills)
        {
            let was_on = self
                .picker_matches()
                .get(self.picker.as_ref().map_or(0, |open| open.selected))
                .and_then(|row| row.label().map(str::to_string));
            let keep_filter = self
                .picker
                .as_ref()
                .map_or_else(String::new, |open| open.filter.clone());
            self.picker = Some(self.skill_list());
            if let Some(pick) = self.picker.as_mut() {
                pick.filter = keep_filter;
            }
            if let Some(was) = was_on.as_ref() {
                let at = self
                    .picker_matches()
                    .iter()
                    .position(|row| row.label() == Some(was.as_str()));
                if let (Some(at), Some(pick)) = (at, self.picker.as_mut()) {
                    pick.selected = at;
                }
            }
        }
    }

    /// Replace the model rows the `/model` list is built from. While the
    /// list is open the rows are its new rows: the user's filter is kept,
    /// and the highlight stays on the `provider/model` it stood on, or
    /// returns to the list's default when that model is gone. The
    /// suggestion popup lists built-ins only, so nothing of its is
    /// touched.
    pub fn set_models(&mut self, models: Arc<Vec<crate::tui::models::ModelChoice>>) {
        self.models = models;
        if self
            .picker
            .as_ref()
            .is_some_and(|open| open.list == crate::tui::picker::List::Models)
        {
            let was_on = self
                .picker_matches()
                .get(self.picker.as_ref().map_or(0, |open| open.selected))
                .and_then(|row| row.label().map(str::to_string));
            let keep_filter = self
                .picker
                .as_ref()
                .map_or_else(String::new, |open| open.filter.clone());
            self.picker = Some(self.model_list());
            if let Some(pick) = self.picker.as_mut() {
                pick.filter = keep_filter;
            }
            if let Some(was) = was_on.as_ref() {
                let at = self
                    .picker_matches()
                    .iter()
                    .position(|row| row.label() == Some(was.as_str()));
                if let (Some(at), Some(pick)) = (at, self.picker.as_mut()) {
                    pick.selected = at;
                }
            }
        }
    }

    // Test seams: the transcript and scroll offset are view-internal; the
    // tests sit inside the module tree and reach in through these.
    #[cfg(test)]
    pub(crate) fn line_kinds_for_test(&self) -> Vec<super::super::transcript::LineKind> {
        self.transcript
            .render()
            .into_iter()
            .map(|l| l.kind)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn line_texts_for_test(&self) -> Vec<String> {
        self.transcript
            .render()
            .into_iter()
            .map(|l| l.text)
            .collect()
    }
}
