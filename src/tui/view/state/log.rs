//! The conversation half of [`State`](super::super::State): what is pushed
//! into the transcript, what a resumed session seeds, what the footer is
//! told about context, tokens and throughput, and what `/new` clears. The
//! struct and its construction are in [`state`](super).

use crate::events::AgentEvent;
use crate::tui::transcript::RenderedLine;

impl super::super::State {
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
        // Throughput belongs to the model that produced it.
        self.footer.set_decode_tps(None);
        self.footer.set_thinking_pct(None);
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
        let (tokens_in, tokens_out) = crate::tui::agent_task::usage_totals(entries);
        self.footer.set_usage(tokens_in, tokens_out);
        // The context figure is the last request's, not the session total's.
        self.set_context_tokens(crate::tui::agent_task::last_context_tokens(entries));
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
        // Nothing has been spent or measured in the new session yet.
        self.last_context_tokens = 0;
        self.footer.set_usage(0, 0);
        self.footer.clear_telemetry();
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
        // A turn's end is where the backend's figures arrive: the context
        // the next request will carry, and how fast this one decoded.
        if let AgentEvent::TurnEnded {
            usage,
            timings,
            thinking_pct,
            ..
        } = event
        {
            if let Some(u) = usage {
                self.set_context_tokens(
                    u64::from(u.prompt_tokens) + u64::from(u.completion_tokens),
                );
            }
            self.footer
                .set_decode_tps(timings.as_ref().and_then(|t| t.decode_tok_per_sec()));
            self.footer.set_thinking_pct(*thinking_pct);
        }
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
    pub(in crate::tui::view) fn lines(&self) -> Vec<RenderedLine> {
        self.transcript.render()
    }

    // Test seams: the transcript and scroll offset are view-internal; the
    // tests sit inside the module tree and reach in through these.
}
