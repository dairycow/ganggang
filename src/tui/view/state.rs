//! The view's state: what the loop, the commands, and the agent's events
//! change between frames — the transcript, the composer, the footer, the
//! scroll offset, the suggestion popup's selection, and the palette picked
//! once at startup. No drawing here and no colour: the geometry is
//! [`layout`](super::super::layout)'s, the style is
//! [`theme`](super::super::theme)'s, and what a line means is the
//! transcript model's.
//! The popup's selection and the run/scroll keys are the other two halves
//! of the same struct: [`popup`] and [`turn`].

mod popup;
mod turn;

pub use turn::Cancel;

use std::sync::Arc;

use super::super::footer::Footer;
use super::super::history::History;
use super::super::input::Editor;
use super::super::theme::Palette;
use super::super::transcript::{RenderedLine, Transcript};
use crate::events::AgentEvent;

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
    /// What `/session` reports; `None` path = not persisted.
    pub session_path: Option<std::path::PathBuf>,
    pub sessions_disabled: bool,
    /// Totals from the last run (§13: summed across the session).
    pub entries: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The last `/resume` listing, for `/resume <n>`.
    pub pending_resume: Vec<crate::agent::session::io::SessionFile>,
    /// The last `/model` listing, for `/model <n>`.
    pub pending_models: Vec<(String, String)>,
    /// Configured context window — the denominator of the footer's %.
    context_window: u64,
    last_context_tokens: u64,
    /// The discovery sets the suggestion popup lists from (§15
    /// amendment): skills for `/skill:<name>` rows, prompt templates
    /// for `/<name>` rows. Shared read-only with the command handler.
    skills: Arc<Vec<crate::agent::skills::Skill>>,
    prompts: Arc<Vec<crate::prompts::PromptTemplate>>,
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
        prompts: Arc<Vec<crate::prompts::PromptTemplate>>,
        history: History,
    ) -> Self {
        Self::with_palette(
            cwd,
            model,
            context_window,
            skills,
            prompts,
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
        prompts: Arc<Vec<crate::prompts::PromptTemplate>>,
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
            session_path: None,
            sessions_disabled: false,
            entries: 0,
            tokens_in: 0,
            tokens_out: 0,
            pending_resume: Vec::new(),
            pending_models: Vec::new(),
            context_window: context_window as u64,
            last_context_tokens: 0,
            skills,
            prompts,
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

    /// A run reported its last request's context usage.
    pub fn set_context_tokens(&mut self, context_tokens: u64) {
        self.last_context_tokens = context_tokens;
        self.footer.set_context(context_tokens, self.context_window);
    }

    pub fn set_model(&mut self, label: &str, context_window: u32) {
        self.footer.set_model(label);
        self.context_window = context_window as u64;
        self.footer
            .set_context(self.last_context_tokens, self.context_window);
    }

    pub fn push_user(&mut self, text: &str) {
        self.transcript.push_user(text);
    }

    pub fn push_info(&mut self, text: &str) {
        self.transcript.push_info(text);
    }

    pub fn push_warning(&mut self, message: &str) {
        self.transcript.apply(&AgentEvent::Warned {
            message: message.to_string(),
        });
    }

    pub fn push_shell(
        &mut self,
        command: &str,
        output: &str,
        exit_code: Option<i32>,
        to_context: bool,
    ) {
        self.transcript
            .push_shell(command, output, exit_code, to_context);
    }

    /// The last assistant text, for Ctrl+X.
    pub fn last_answer(&self) -> Option<String> {
        self.transcript.last_answer().map(String::from)
    }

    /// Whether the session has nothing to show yet — the transcript empty
    /// and nothing carried over from a session file. That, not a scroll
    /// position or a draft, is what the welcome block answers.
    pub fn is_quiet(&self) -> bool {
        self.transcript.is_empty()
    }

    /// Every path that starts work: busy flag + footer indicator, and no
    /// stopping mark left over from a cancel the last run did not need.
    pub fn begin_run(&mut self) {
        self.busy = true;
        self.interrupt_armed = false;
        self.footer.set_busy(true);
        self.footer.set_stopping(false);
    }

    pub fn record_totals(&mut self, tokens_in: u64, tokens_out: u64, entries: usize) {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self.entries = entries;
    }

    /// Seed the view from persisted entries (resume): transcript blocks
    /// from the disk-known conversation, footer totals from its usage.
    /// Without this a resumed session renders blank and reports 0 tokens
    /// until the first new run.
    pub fn seed_from_entries(&mut self, entries: &[crate::agent::session::Entry]) {
        self.transcript.extend_from_entries(entries);
        let (tokens_in, tokens_out) = super::super::agent_task::usage_totals(entries);
        self.footer.set_usage(tokens_in, tokens_out);
        self.record_totals(tokens_in, tokens_out, entries.len());
    }

    /// `/new` / `/resume`: the view starts describing the new session.
    /// The transcript keeps prior content — it is what the human saw.
    pub fn new_session(
        &mut self,
        session_path: Option<std::path::PathBuf>,
        sessions_disabled: bool,
    ) {
        self.session_path = session_path;
        self.sessions_disabled = sessions_disabled;
        self.entries = 0;
        self.tokens_in = 0;
        self.tokens_out = 0;
        // A recall from the old session has nothing to do with the new one;
        // a Ctrl+C that was waiting to be confirmed is over too.
        self.history.reset();
        self.set_stopping(false);
        self.interrupt_armed = false;
        let where_ = self
            .session_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not persisted".into());
        self.transcript
            .push_info(&format!("— new session — {where_}"));
    }

    /// Run-level failures render as warnings in the transcript — the same
    /// shape a `Warned` event uses.
    pub fn push_failure(&mut self, error: &str) {
        self.transcript.apply(&AgentEvent::Warned {
            message: format!("run failed: {error}"),
        });
    }

    /// Command failures (a bad /resume index, a missing skill) are local
    /// mistakes, not run failures — their own shape, no alarm prefix.
    pub fn push_command_error(&mut self, message: &str) {
        self.transcript.push_command_error(message);
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        self.transcript.apply(event);
    }

    /// Ctrl+O: expand the most recent collapsed call; when all are
    /// expanded, collapse them all (the transcript's documented rule).
    pub fn toggle_expand(&mut self) {
        self.transcript.toggle_expand();
    }

    /// Ctrl+T: show/hide the model's thinking (stored either way).
    pub fn toggle_thinking(&mut self) {
        self.transcript.toggle_thinking();
    }

    /// The transcript's rendered lines, for the draw pass and for the
    /// scroll clamp's lower bound.
    pub(super) fn lines(&self) -> Vec<RenderedLine> {
        self.transcript.render()
    }

    /// The spinner's next frame, called from the event loop's 100 ms timer
    /// arm — which the loop arms only while a run is live, so an idle view
    /// never wakes a frame on a timer.
    pub fn tick_spinner(&mut self) {
        self.footer.tick();
    }

    /// Whether anything is turning a spinner right now.
    pub fn spinning(&self) -> bool {
        self.footer.spinning()
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
