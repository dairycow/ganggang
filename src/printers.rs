//! Print-mode rendering (spec §15): progress lines to stderr while a run
//! is live, the final assistant text alone to stdout — so the answer
//! stays pipeable (`git diff | gg -p "review"`).
//!
//! The printer is pure: [`Printer::event`] returns the stderr lines an
//! event renders, [`Printer::answer`] the stdout text at the end; the
//! caller does the actual writing. That keeps rendering testable without
//! a terminal.

use crate::events::AgentEvent;

/// Spinner frames for streamed text (§15: progress to stderr only).
const SPIN_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

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
            AgentEvent::RunStarted | AgentEvent::RunEnded { .. } | AgentEvent::RunAborted => {
                Vec::new()
            }
            AgentEvent::TurnStarted => {
                self.close_turn();
                self.turn += 1;
                vec![format!("── turn {} ──", self.turn)]
            }
            AgentEvent::TurnEnded { .. } => Vec::new(),
            // §15, M7.3: print mode never shows thinking — not even a
            // marker; stdout stays the answer, stderr stays progress.
            AgentEvent::Thinking { .. } => Vec::new(),
            AgentEvent::AssistantText { delta } => {
                self.current.push_str(&delta);
                if !self.spin {
                    return Vec::new();
                }
                let frame = SPIN_FRAMES[self.spinner % SPIN_FRAMES.len()];
                self.spinner = self.spinner.wrapping_add(1);
                self.spinner_open = true;
                vec![format!(
                    "\r{frame} streaming… {} chars",
                    self.current.chars().count()
                )]
            }
            AgentEvent::ToolCallStarted { name, args, .. } => {
                vec![format!("▸ {name} {}", one_line(&args.to_string(), 80))]
            }
            AgentEvent::ToolCallFinished {
                name,
                output,
                is_error,
                ..
            } => {
                if is_error {
                    vec![format!("✗ {name} — {}", one_line(&output, 100))]
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
        if !self.current.is_empty() {
            self.answer = std::mem::take(&mut self.current);
        }
    }
}

/// Collapse to one line and cap the length, marking a cut with `…` —
/// tool output and args can be huge, progress lines must not be.
fn one_line(text: &str, cap: usize) -> String {
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
mod tests {
    use super::*;
    use serde_json::json;

    fn render(p: &mut Printer, events: Vec<AgentEvent>) -> Vec<String> {
        events.into_iter().flat_map(|e| p.event(e)).collect()
    }

    #[test]
    fn plain_answer_progress_to_stderr_answer_at_end() {
        let mut p = Printer::new();
        let lines = render(
            &mut p,
            vec![
                AgentEvent::RunStarted,
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "0.1.0".into(),
                },
                AgentEvent::TurnEnded {
                    stop_reason: "stop".into(),
                },
                AgentEvent::RunEnded { turns: 1 },
            ],
        );

        // The turn marker, one raw spinner update for the text, and the
        // close marker that erases it before the stream goes quiet.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "── turn 1 ──");
        assert!(
            lines[1].starts_with("\r⠋ streaming… 5 chars"),
            "{}",
            lines[1]
        );
        assert_eq!(lines[2], "\r\u{1b}[2K\n");
        // Nothing renders to stdout mid-run; the answer is pulled once
        // at the end.
        assert_eq!(p.answer(), "0.1.0");
    }

    /// Consecutive deltas each overwrite the spinner line in place (raw
    /// '\r' lines, no newline of their own); the next non-delta event
    /// closes it exactly once, and the lines after the close are clean.
    #[test]
    fn streamed_deltas_overwrite_in_place_and_close_once() {
        let mut p = Printer::new();
        let lines = render(
            &mut p,
            vec![
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "foo ".into(),
                },
                AgentEvent::AssistantText {
                    delta: "bar".into(),
                },
                AgentEvent::ToolCallStarted {
                    name: "read".into(),
                    id: "c1".into(),
                    args: json!({"path": "Cargo.toml"}),
                },
            ],
        );

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "── turn 1 ──");
        assert!(
            lines[1].starts_with("\r⠋ streaming… 4 chars"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("\r⠙ streaming… 7 chars"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "\r\u{1b}[2K\n");
        assert_eq!(lines[4], "▸ read {\"path\":\"Cargo.toml\"}");
        // No later line carries spinner debris, and an unterminated
        // streamed turn still yields its partial text as the answer.
        assert_eq!(p.answer(), "foo bar");
    }

    #[test]
    fn tool_run_shows_call_lines_and_the_final_answer() {
        let mut p = Printer::new();
        let lines = render(
            &mut p,
            vec![
                AgentEvent::RunStarted,
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "checking".into(),
                },
                AgentEvent::ToolCallStarted {
                    name: "read".into(),
                    id: "c1".into(),
                    args: json!({"path": "Cargo.toml"}),
                },
                AgentEvent::ToolCallFinished {
                    name: "read".into(),
                    id: "c1".into(),
                    output: "…file contents…".into(),
                    is_error: false,
                },
                AgentEvent::ToolCallStarted {
                    name: "bash".into(),
                    id: "c2".into(),
                    args: json!({"command": "cargo test"}),
                },
                AgentEvent::ToolCallFinished {
                    name: "bash".into(),
                    id: "c2".into(),
                    output: "error[E0308]: mismatched\ntypes".into(),
                    is_error: true,
                },
                AgentEvent::TurnEnded {
                    stop_reason: "tool_calls".into(),
                },
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "one test fails".into(),
                },
                AgentEvent::TurnEnded {
                    stop_reason: "stop".into(),
                },
                AgentEvent::RunEnded { turns: 2 },
            ],
        );

        let err = lines;
        assert!(err.contains(&"── turn 1 ──".to_string()));
        assert!(err.contains(&"── turn 2 ──".to_string()));
        assert!(
            err[1].starts_with("\r⠋ streaming… 8 chars"),
            "the narration streams a spinner line: {}",
            err[1]
        );
        assert_eq!(
            err[2], "\r\u{1b}[2K\n",
            "the spinner closes before the tool line"
        );
        assert_eq!(
            err[3], "▸ read {\"path\":\"Cargo.toml\"}",
            "tool start shows compact args"
        );
        assert_eq!(err[4], "✓ read");
        assert_eq!(
            err[6], "✗ bash — error[E0308]: mismatched types",
            "error finish carries a one-line excerpt"
        );
        // The answer is the FINAL turn's text, not the early narration.
        assert_eq!(p.answer(), "one test fails");
    }

    #[test]
    fn warnings_render_prefixed() {
        let mut p = Printer::new();
        let lines = render(
            &mut p,
            vec![AgentEvent::Warned {
                message: "response truncated".into(),
            }],
        );
        assert_eq!(lines, vec!["warning: response truncated"]);
    }

    #[test]
    fn aborted_run_still_yields_the_last_text() {
        let mut p = Printer::new();
        render(
            &mut p,
            vec![
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "partial work".into(),
                },
                AgentEvent::Warned {
                    message: "run aborted".into(),
                },
            ],
        );
        assert_eq!(p.answer(), "partial work");
    }

    /// Print mode omits thinking entirely (§15, M7.3) — no marker, no
    /// line — and a thinking fragment keeps the spinner line alive the
    /// way a text delta does (progress is progress; the spinner should
    /// not flap while the model works through its reasoning).
    #[test]
    fn thinking_renders_nothing_and_keeps_the_spinner_alive() {
        let mut p = Printer::new();
        let lines = render(
            &mut p,
            vec![
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "ans".into(),
                },
                AgentEvent::Thinking {
                    delta: "internally reasoning".into(),
                },
                AgentEvent::Thinking {
                    delta: " more".into(),
                },
                AgentEvent::TurnEnded {
                    stop_reason: "stop".into(),
                },
            ],
        );
        // Turn marker, one spinner update (the text), then the close on
        // TurnEnded — the thinking events contributed nothing.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "── turn 1 ──");
        assert!(
            lines[1].starts_with("\r⠋ streaming… 3 chars"),
            "{}",
            lines[1]
        );
        assert_eq!(lines[2], "\r\u{1b}[2K\n");
        // Thinking never reaches the answer either.
        assert_eq!(p.answer(), "ans");
    }

    /// No TTY on stderr ⇒ deltas render nothing at all: no spinner, no
    /// ANSI, nothing for a redirected stream to choke on. Other progress
    /// lines are unaffected.
    #[test]
    fn quiet_spinner_emits_nothing_for_deltas() {
        let mut p = Printer::new().with_spinner(false);
        let lines = render(
            &mut p,
            vec![
                AgentEvent::TurnStarted,
                AgentEvent::AssistantText {
                    delta: "hello ".into(),
                },
                AgentEvent::AssistantText {
                    delta: "world".into(),
                },
                AgentEvent::ToolCallStarted {
                    name: "read".into(),
                    id: "c1".into(),
                    args: json!({"path": "Cargo.toml"}),
                },
            ],
        );
        assert_eq!(
            lines,
            vec![
                "── turn 1 ──".to_string(),
                "▸ read {\"path\":\"Cargo.toml\"}".to_string()
            ]
        );
        // No spinner was ever open, so closing emits nothing either.
        assert!(p.close().is_empty());
        assert_eq!(p.answer(), "hello world");
    }

    /// A run that dies mid-stream ends the channel with the spinner
    /// still open: `close` erases it once, and is a no-op after that.
    #[test]
    fn close_erases_a_spinner_left_open_by_a_dead_run() {
        let mut p = Printer::new();
        render(
            &mut p,
            vec![AgentEvent::AssistantText {
                delta: "half an answ".into(),
            }],
        );
        assert_eq!(p.close(), vec!["\r\u{1b}[2K\n".to_string()]);
        assert!(p.close().is_empty());
    }
}
