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
//! scrolling), [`popup`] the suggestion list's selection and which popup
//! has the arrows, and [`picker`] the `/session` list.

mod log;
mod picker;
mod popup;
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
    /// The last `/model` listing, for `/model <n>`.
    pub pending_models: Vec<(String, String)>,
    /// The `/session` list while it is open; the text after the command
    /// is its filter. `None` = closed.
    pub picker: Option<crate::tui::picker::Picker>,
    /// Configured context window — the denominator of the footer's %.
    context_window: u64,
    last_context_tokens: u64,
    /// The discovery sets the suggestion popup lists from (§15
    /// amendment): skills for `/skill:<name>` rows. Shared read-only
    /// with the command handler.
    skills: Arc<Vec<crate::agent::skills::Skill>>,
    /// The model rows the `/model` popup lists from, and the active
    /// model's `provider/model` label, so its row reads `· active`.
    /// Shared read-only with the command handler, like `skills`.
    models: Arc<Vec<crate::tui::suggest::ModelChoice>>,
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
            pending_models: Vec::new(),
            picker: None,
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

    /// The shared skill set, as an `Arc`: what a `/reload` replaces and
    /// what the command handler snapshots for its lists.
    pub fn skills_arc(&self) -> Arc<Vec<crate::agent::skills::Skill>> {
        self.skills.clone()
    }

    /// Replace the skill set (a `/reload` or a swapped agent) and reset
    /// the popup's selection: the highlighted row and filter are stale
    /// against a new list.
    pub fn set_skills(&mut self, skills: Arc<Vec<crate::agent::skills::Skill>>) {
        self.skills = skills;
        self.suggest_selected = 0;
        self.suggest_filter.clear();
    }

    /// Replace the model rows the `/model` popup lists from and reset the
    /// popup's selection, as [`set_skills`](Self::set_skills) does.
    pub fn set_models(&mut self, models: Arc<Vec<crate::tui::suggest::ModelChoice>>) {
        self.models = models;
        self.suggest_selected = 0;
        self.suggest_filter.clear();
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

    #[cfg(test)]
    pub(crate) fn suggest_selected_for_test(&self) -> usize {
        self.suggest_selected
    }
}
