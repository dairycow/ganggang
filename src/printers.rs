//! Print-mode rendering (spec §15): progress lines to stderr while a run
//! is live, the final assistant text alone to stdout — so the answer
//! stays pipeable (`git diff | gg -p "review"`).
//!
//! The printer is pure: [`Printer::event`] returns the stderr lines an
//! event renders, [`Printer::answer`] the stdout text at the end; the
//! caller does the actual writing. That keeps rendering testable without
//! a terminal.

use crate::events::AgentEvent;
use crate::tool_args::format_tool_args;

/// Args shown on a print-mode `▸ name args` line: long enough for a command
/// to read, capped so progress stays line-oriented.
const ARGS_CHARS: usize = 180;

/// Spinner frames for streamed text (§15: progress to stderr only): the
/// footer's mark, so both surfaces spin alike.
use crate::tui::footer::MARK_FRAMES;

/// Erases an open spinner line and terminates it. Like the updates it
/// starts with '\r' (the caller's raw-write marker) but carries its own
/// newline, so the next real line starts clean and line-oriented stderr
/// consumers never see progress glued onto a tool line.
const SPIN_CLOSE: &str = "\r\x1b[2K\n";

/// Accumulates a print-mode run: turn markers, tool-call lines, warnings
/// and a streamed-text spinner (all stderr, emitted as they happen) and
/// the answer (stdout, once, at the end).
///
/// Streaming contract with the caller: lines that start with '\r' are
/// written RAW (no added newline) so each spinner update overwrites the
/// last in place; every other line is a plain stderr line.
pub struct Printer {
    turn: u32,
    /// Assistant text of the turn in progress (streaming: many deltas).
    current: String,
    /// The last non-empty assistant text. With tool use, early turns
    /// often narrate ("let me check…"); the *answer* is the final one.
    answer: String,
    spinner: usize,
    /// A spinner line is currently open on stderr (raw, unterminated).
    spinner_open: bool,
    /// Spinner updates are in-place ANSI redraws; they belong on a
    /// terminal, not in a redirected/piped stderr. Off ⇒ deltas render
    /// nothing (well-behaved-CLI rule; other progress lines stay).
    spin: bool,
    /// The last turn ended short (cut stream or `length`) and the loop is
    /// continuing it, so the next turn's deltas finish THIS answer —
    /// print mode's stdout must show one answer, not just the tail.
    continued: bool,
    tool_calls: u32,
    tool_errors: u32,
    peak_prompt_tokens: u32,
    total_prompt_tokens: u64,
    total_completion_tokens: u64,
    /// Cache-read tokens across the run: the summary's `cache NN%` hit
    /// rate divides this by the prompt total.
    total_cache_read_tokens: u64,
    total_predicted_n: u64,
    total_predicted_ms: f64,
    total_reasoning_tokens: u64,
    last_decode_tps: Option<f64>,
    last_thinking_pct: Option<u8>,
}

impl Default for Printer {
    fn default() -> Self {
        Self::new()
    }
}

impl Printer {
    pub fn new() -> Self {
        Self {
            turn: 0,
            current: String::new(),
            answer: String::new(),
            spinner: 0,
            spinner_open: false,
            spin: true,
            continued: false,
            tool_calls: 0,
            tool_errors: 0,
            peak_prompt_tokens: 0,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            total_cache_read_tokens: 0,
            total_predicted_n: 0,
            total_predicted_ms: 0.0,
            total_reasoning_tokens: 0,
            last_decode_tps: None,
            last_thinking_pct: None,
        }
    }

    /// Gate the streaming spinner (callers pass `stderr().is_terminal()`).
    pub fn with_spinner(mut self, on: bool) -> Self {
        self.spin = on;
        self
    }

    /// Consume one event; returns the stderr lines to write now
    /// (possibly none — stdout only ever carries the final answer).
    pub fn event(&mut self, event: AgentEvent) -> Vec<String> {
        let was_streaming = self.spinner_open;
        // A further delta keeps the spinner line open; everything else
        // replaces it. Thinking counts as progress too (spec §15: print
        // mode omits thinking; the spinner should not flap while the
        // model works through it).
        let continues_spinner = matches!(
            event,
            AgentEvent::AssistantText { .. } | AgentEvent::Thinking { .. }
        );
        let mut lines = self.render(event);
        // The close marker erases the spinner line first so real lines
        // stay clean for line-oriented stderr consumers.
        if was_streaming && !continues_spinner {
            self.spinner_open = false;
            lines.insert(0, SPIN_CLOSE.to_string());
        }
        lines
    }

    fn render(&mut self, event: AgentEvent) -> Vec<String> {
        match event {
            AgentEvent::RunStarted => Vec::new(),
            AgentEvent::RunEnded { turns } => {
                if turns == 0 {
                    return Vec::new();
                }
                let parts = self.summary_parts(turns);
                vec![format!("── done: {} ──", parts.join(" · "))]
            }
            AgentEvent::RunAborted => {
                let parts = self.summary_parts(self.turn);
                vec![format!("── aborted after {} ──", parts.join(" · "))]
            }
            AgentEvent::TurnStarted => {
                self.close_turn();
                self.turn += 1;
                vec![format!("── turn {} ──", self.turn)]
            }
            AgentEvent::TurnEnded {
                stop_reason,
                usage,
                timings,
                thinking_pct,
                ..
            } => {
                // Stop reasons that mean "this answer was cut" (see
                // `events::STOP_*`): the loop is continuing it, so the text
                // accumulated so far must survive the turn boundary.
                self.continued = matches!(
                    stop_reason.as_str(),
                    crate::events::STOP_LENGTH | crate::events::STOP_STREAM_CUT
                );
                let mut parts = Vec::new();
                if let Some(u) = usage {
                    self.peak_prompt_tokens = self.peak_prompt_tokens.max(u.prompt_tokens);
                    self.total_prompt_tokens += u.prompt_tokens as u64;
                    self.total_completion_tokens += u.completion_tokens as u64;
                    self.total_cache_read_tokens += u.cache_read_tokens as u64;
                    if let Some(r) = u.reasoning_tokens {
                        self.total_reasoning_tokens += r as u64;
                    }
                    parts.push(format!("ctx {} tok", u.prompt_tokens));
                    parts.push(format!("gen {} tok", u.completion_tokens));
                }
                if let Some(t) = timings.as_ref() {
                    if let (Some(n), Some(ms)) = (t.predicted_n, t.predicted_ms)
                        && n > 0
                        && ms.is_finite()
                        && ms > 0.0
                    {
                        self.total_predicted_n += n as u64;
                        self.total_predicted_ms += ms;
                    }
                    if let Some(tps) = t.decode_tok_per_sec() {
                        self.last_decode_tps = Some(tps);
                        parts.push(format!("{tps:.1} tok/s"));
                    }
                }
                if let Some(pct) = thinking_pct {
                    self.last_thinking_pct = Some(pct);
                    parts.push(format!("think {pct}%"));
                }
                if parts.is_empty() {
                    Vec::new()
                } else {
                    vec![format!("  {}", parts.join(" · "))]
                }
            }
            // §15, M7.3: print mode never shows thinking — not even a
            // marker; stdout stays the answer, stderr stays progress.
            AgentEvent::Thinking { .. } => Vec::new(),
            AgentEvent::AssistantText { delta } => {
                self.current.push_str(&delta);
                if !self.spin {
                    return Vec::new();
                }
                // The same mark the footer draws, so print mode and the
                // TUI say the same thing while a run works.
                let (left, right) = MARK_FRAMES[self.spinner % MARK_FRAMES.len()];
                let frame = format!("{left}{right}");
                self.spinner = self.spinner.wrapping_add(1);
                self.spinner_open = true;
                vec![format!(
                    "\r{frame} streaming… {} chars",
                    self.current.chars().count()
                )]
            }
            AgentEvent::ToolCallStarted { name, args, .. } => {
                self.tool_calls += 1;
                vec![format!(
                    "▸ {name} {}",
                    format_tool_args(&name, &args, ARGS_CHARS)
                )]
            }
            AgentEvent::ToolCallFinished {
                name,
                output,
                is_error,
                ..
            } => {
                if is_error {
                    self.tool_errors += 1;
                    vec![format!("✗ {name} — {}", one_line(&output, 140))]
                } else {
                    vec![format!("✓ {name}")]
                }
            }
            AgentEvent::Warned { message } => vec![format!("warning: {message}")],
        }
    }

    /// The final answer for stdout. Called after the run returns — also
    /// on abort, where a partial answer beats nothing.
    pub fn answer(&self) -> &str {
        if self.current.is_empty() {
            &self.answer
        } else {
            &self.current
        }
    }

    /// Erase a still-open spinner line. Call when the event channel
    /// ends — a run that dies mid-stream (transport error) never sends
    /// the next event that would otherwise close it, and the error
    /// report must not glue onto spinner residue.
    pub fn close(&mut self) -> Vec<String> {
        if self.spinner_open {
            self.spinner_open = false;
            vec![SPIN_CLOSE.to_string()]
        } else {
            Vec::new()
        }
    }

    fn close_turn(&mut self) {
        if self.continued {
            // A continuation: this turn's deltas finish the previous
            // answer, so `current` stays where it is and the pieces
            // accumulate into one answer.
            self.continued = false;
            return;
        }
        if !self.current.is_empty() {
            self.answer = std::mem::take(&mut self.current);
        }
    }

    fn summary_parts(&self, turns: u32) -> Vec<String> {
        let mut parts = vec![
            format!("{turns} turn(s)"),
            format!(
                "{} tool(s){}",
                self.tool_calls,
                if self.tool_errors > 0 {
                    format!(" ({} err)", self.tool_errors)
                } else {
                    String::new()
                }
            ),
        ];
        if self.peak_prompt_tokens > 0 {
            parts.push(format!("peak ctx {} tok", self.peak_prompt_tokens));
        }
        if self.total_cache_read_tokens > 0 && self.total_prompt_tokens > 0 {
            let pct = (self.total_cache_read_tokens.saturating_mul(100) / self.total_prompt_tokens)
                .min(100);
            parts.push(format!("cache {pct}%"));
        }
        if self.total_completion_tokens > 0 {
            let agg_tps = (self.total_predicted_n > 0 && self.total_predicted_ms > 0.0)
                .then(|| self.total_predicted_n as f64 / (self.total_predicted_ms / 1000.0))
                .or(self.last_decode_tps);
            let tps = agg_tps
                .map(|t| format!(" ({t:.1} tok/s)"))
                .unwrap_or_default();
            parts.push(format!("gen {} tok{tps}", self.total_completion_tokens));
        }
        let think = if self.total_reasoning_tokens > 0 && self.total_completion_tokens > 0 {
            Some(
                ((self.total_reasoning_tokens * 100) / self.total_completion_tokens).min(100) as u8,
            )
        } else {
            self.last_thinking_pct
        };
        if let Some(pct) = think {
            parts.push(format!("think {pct}%"));
        }
        parts
    }
}

/// Collapse to one line and cap the length, marking a cut with `…` —
/// tool output and args can be huge, progress lines must not be.
pub(super) fn one_line(text: &str, cap: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= cap {
        flat
    } else {
        let cut: String = flat.chars().take(cap).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests;
