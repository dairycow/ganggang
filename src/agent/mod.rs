//! The harness proper: the session model, tools, and the agent loop.
//!
//! Nothing in this layer touches clap, ratatui, or stdout — decisions only.
//! That keeps it testable against a fake server with no UI in the way.

pub mod compaction;
pub mod context;
pub mod hooks;
pub mod loop_;
pub mod session;
pub mod skills;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::llm::client::LlmClient;

/// One conversation under run: the model endpoint, the growing entry log,
/// the tool surface, and the flag an interface sets to stop between tool
/// calls. `run` (in [`loop_`]) is the only driver.
pub struct Agent {
    pub(super) client: LlmClient,
    pub(super) entries: Vec<session::Entry>,
    pub(super) tools: tools::Registry,
    pub(super) model: String,
    pub(super) model_label: Option<String>,
    pub(super) max_tokens: u32,
    pub(super) abort: Arc<AtomicBool>,
    /// Session cwd: tools run here and the system prompt reports it.
    pub(super) cwd: PathBuf,
    /// Context files discovered at construction; the system prompt is
    /// reassembled from them on every request (spec §12).
    pub(super) agents_files: Vec<context::AgentsFile>,
    /// Unreadable-but-present context files, surfaced as `Warned` events
    /// on the first run (§17: never silently dropped).
    pub(super) agents_warnings: Vec<String>,
    /// Skills discovered at construction (first-wins, with warnings for
    /// collisions and skips) — advertised by name in the system prompt.
    pub(super) skills: Vec<skills::Skill>,
    /// Per-skill discovery warnings, surfaced as `Warned` events on the
    /// first run so silent skips stay visible (§17).
    pub(super) skill_warnings: Vec<String>,
    /// Where entries are persisted as they are appended; `None` is
    /// `--no-session`. Session I/O errors abort the run (§17).
    pub(super) store: Option<session::io::SessionStore>,
    /// Stream responses (M5): text deltas reach the event channel as they
    /// arrive. Interfaces set it true; loop tests script the simpler
    /// non-streaming fake server, which stays the replayed path.
    pub(super) stream: bool,
    /// The compaction ceiling (§14, config `context_window`): the run
    /// compacts once the conversation crowds past window − reserve.
    pub(super) context_window: u32,
    /// Optional lower compaction threshold (e.g. local VRAM KV cliff).
    pub(super) compact_threshold_tokens: Option<u32>,
    /// Optional per-run turn budget (`--max-turns` / `GG_MAX_TURNS`).
    pub(super) max_turns: Option<u32>,
    /// Frozen UTC date (`YYYY-MM-DD`) captured at Agent creation so
    /// midnight UTC never invalidates the KV prefix mid-session.
    pub(super) prompt_date: String,
    /// Whether replayed assistant messages must include `reasoning_content`.
    pub(super) replay_reasoning: session::ReplayReasoning,
    /// Stable per-agent session id for provider routing headers (e.g. OpenCode Go).
    pub(super) session_id: String,
    /// User-level hooks (§22). Loaded once per process; a run consults them
    /// at the tool and run boundaries.
    pub(super) hooks: hooks::Hooks,
}

impl Agent {
    /// One flat construction list — every parameter has a distinct owner
    /// story (endpoint, tool surface, limits, session, transport mode,
    /// compaction ceiling) and a builder would only hide the same
    /// decisions behind punctuation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: LlmClient,
        tools: tools::Registry,
        model: String,
        max_tokens: u32,
        abort: Arc<AtomicBool>,
        cwd: PathBuf,
        store: Option<session::io::SessionStore>,
        stream: bool,
        context_window: u32,
        hooks: hooks::Hooks,
    ) -> Self {
        let (agents_files, agents_warnings) = context::discover_agents_md(&cwd);
        let (skills, skill_warnings) = skills::discover(&cwd);
        Self {
            client,
            entries: Vec::new(),
            tools,
            model,
            model_label: None,
            max_tokens,
            abort,
            cwd,
            agents_files,
            agents_warnings,
            skills,
            skill_warnings,
            store,
            stream,
            context_window,
            compact_threshold_tokens: None,
            max_turns: None,
            prompt_date: context::today_utc_iso(),
            replay_reasoning: session::ReplayReasoning::Never,
            session_id: uuid::Uuid::new_v4().hyphenated().to_string(),
            hooks,
        }
    }

    pub fn with_replay_reasoning(mut self, replay: session::ReplayReasoning) -> Self {
        self.replay_reasoning = replay;
        self
    }

    pub fn with_compact_threshold(mut self, threshold: Option<u32>) -> Self {
        self.compact_threshold_tokens = threshold.filter(|&t| t > 0);
        self
    }

    pub fn with_max_turns(mut self, max_turns: Option<u32>) -> Self {
        self.max_turns = max_turns.filter(|&n| n > 0);
        self
    }

    pub fn with_model_label(mut self, label: impl Into<String>) -> Self {
        self.model_label = Some(label.into());
        self
    }

    pub fn switch_endpoint(&mut self, endpoint: &crate::config::Endpoint) -> anyhow::Result<()> {
        // The retry budget and the request timeout are the run's, not the
        // endpoint's: carry both onto the fresh client rather than falling
        // back to the defaults.
        let client = LlmClient::from_endpoint(endpoint)?
            .with_session_id(self.session_id.clone())
            .with_retry(self.client.max_retries())
            .with_request_timeout(self.client.request_timeout_secs());
        self.client = client;
        self.model = endpoint.model.clone();
        self.model_label = Some(endpoint.label());
        self.max_tokens = endpoint.max_tokens;
        self.context_window = endpoint.context_window;
        self.replay_reasoning = endpoint.replay_reasoning;
        self.compact_threshold_tokens = endpoint.compact_threshold_tokens;
        Ok(())
    }

    /// Seed the history from a loaded session (resume). New entries chain
    /// onto the last loaded one.
    pub fn load_entries(&mut self, entries: Vec<session::Entry>) {
        self.entries = entries;
    }

    /// The system prompt the next request carries (§12) — the loop builds
    /// each request through this, and the TUI's `/context` shows exactly
    /// the same assembly.
    pub fn system_prompt(&self) -> String {
        let tool_names: Vec<&str> = self.tools.tools().iter().map(|t| t.name()).collect();
        context::build_system_prompt_for(
            &self.cwd,
            &self.agents_files,
            &self.skills,
            &self.prompt_date,
            Some(self.model_label.as_deref().unwrap_or(&self.model)),
            &tool_names,
        )
    }

    /// The discovered skills, in discovery order: what the TUI's `/skills`
    /// list and `/skill:` rows render after a `/reload`.
    pub fn skills(&self) -> &[skills::Skill] {
        &self.skills
    }

    /// Re-run discovery of context files and skills from disk and swap the
    /// results in: the session's entries, session id, and frozen prompt
    /// date survive, so a reload never invalidates the conversation.
    /// Returns the new counts and the discovery warnings (skill warnings
    /// first, then context-file warnings); the stored warning lists are
    /// cleared, so a reload does not re-surface the first run's.
    pub fn reload_context(&mut self) -> (usize, usize, Vec<String>) {
        let (agents_files, agents_warnings) = context::discover_agents_md(&self.cwd);
        let (skills, skill_warnings) = skills::discover(&self.cwd);
        self.agents_files = agents_files;
        self.skills = skills;
        self.skill_warnings.clear();
        self.agents_warnings.clear();
        let mut warnings = skill_warnings;
        warnings.extend(agents_warnings);
        (self.agents_files.len(), self.skills.len(), warnings)
    }

    /// The recorded history: usage totals, compaction walks, and the
    /// resume path all read through this.
    pub fn entries(&self) -> &[session::Entry] {
        &self.entries
    }

    pub fn replay_reasoning(&self) -> session::ReplayReasoning {
        self.replay_reasoning
    }

    /// Ensure the session file exists. `--watch` restarts resume by id, so
    /// the file has to be written before the first message; ordinary runs
    /// stay lazy (§13). No-op with `--no-session`.
    pub fn ensure_session_file(&mut self) -> anyhow::Result<()> {
        match &mut self.store {
            Some(store) => store.materialize(),
            None => Ok(()),
        }
    }

    /// The session's header record — the line JSON mode echoes before
    /// its event stream (§15). `Ok(None)` when the run keeps no session;
    /// a read failure is the caller's diagnostic to report (JSON mode
    /// warns on stderr and starts at the events).
    pub fn session_header_record(&self) -> anyhow::Result<Option<String>> {
        match &self.store {
            Some(store) => Ok(Some(store.header_record()?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
