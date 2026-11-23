//! Rendering the transcript model: blocks become styled lines, tool
//! calls collapse or expand, everything stays data — no terminal here.

use super::markdown;
use super::{Block, EXPAND_LINES, SHELL_LINES, THINKING_LINES};

/// How a rendered line is styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    /// Inside a fenced code block (M7.6): distinct color, no markdown.
    Code,
    Tool,
    ToolError,
    Dim,
}

/// One rendered line: text plus how to style it, and — for markdown
/// bold spans — the char ranges (into `text`) to emphasize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    pub text: String,
    pub kind: LineKind,
    pub bold: Vec<(usize, usize)>,
}

impl RenderedLine {
    pub fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
            bold: Vec::new(),
        }
    }

    /// Attach bold char-ranges (the markdown pass).
    pub fn with_bold(mut self, bold: Vec<(usize, usize)>) -> Self {
        self.bold = bold;
        self
    }
}

impl super::Transcript {
    /// The renderable lines. Expanded calls show their output capped at
    /// [`EXPAND_LINES`] lines with a visible truncation marker.
    pub fn render(&self) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        for block in &self.blocks {
            match block {
                Block::User { text } => {
                    for line in text.split('\n') {
                        lines.push(RenderedLine::new(format!("you │ {line}"), LineKind::User));
                    }
                }
                // Assistant text renders as minimal markdown (M7.6);
                // user text is what the user typed and stays literal.
                Block::Assistant { text } => lines.extend(markdown::lines(text)),
                // Hidden unless Ctrl+T; capped like every other expanded
                // block, with the same honest truncation marker.
                Block::Thinking { text } if self.show_thinking => {
                    let total = text.lines().count();
                    for line in text.lines().take(THINKING_LINES) {
                        lines.push(RenderedLine::new(format!("~ {line}"), LineKind::Dim));
                    }
                    if total > THINKING_LINES {
                        lines.push(RenderedLine::new(
                            format!("~ [truncated: {total} lines]"),
                            LineKind::Dim,
                        ));
                    }
                }
                Block::Thinking { .. } => {}
                Block::ToolCall(call) => {
                    let status = match (&call.output, call.is_error) {
                        (None, _) => "",
                        (Some(_), true) => " ✗",
                        (Some(_), false) => " ✓",
                    };
                    let kind = if call.is_error {
                        LineKind::ToolError
                    } else {
                        LineKind::Tool
                    };
                    lines.push(RenderedLine::new(
                        format!(
                            "{} {} {}{}",
                            if call.expanded { "▾" } else { "▸" },
                            call.name,
                            call.args_display,
                            status
                        ),
                        kind,
                    ));
                    if call.expanded
                        && let Some(output) = &call.output
                    {
                        let total = output.lines().count();
                        for line in output.lines().take(EXPAND_LINES) {
                            lines.push(RenderedLine::new(format!("  {line}"), LineKind::Dim));
                        }
                        if total > EXPAND_LINES {
                            lines.push(RenderedLine::new(
                                format!("  [truncated: {total} lines]"),
                                LineKind::Dim,
                            ));
                        }
                    }
                }
                Block::Warning { message } => {
                    // A warning may carry a multi-line diagnostic (cargo's
                    // failure tail); keep the lines apart so rustc's own
                    // alignment survives instead of becoming blank cells.
                    for (index, line) in message.split('\n').enumerate() {
                        let text = if index == 0 {
                            format!("⚠ {line}")
                        } else {
                            format!("  {line}")
                        };
                        lines.push(RenderedLine::new(text, LineKind::Dim));
                    }
                }
                Block::Info { text } => {
                    for line in text.split('\n') {
                        lines.push(RenderedLine::new(line, LineKind::Dim));
                    }
                }
                Block::CommandError { message } => {
                    lines.push(RenderedLine::new(format!("✗ {message}"), LineKind::Dim));
                }
                Block::Shell {
                    command,
                    output,
                    exit_code,
                    to_context,
                } => {
                    let code = match exit_code {
                        Some(code) => code.to_string(),
                        None => "killed by signal".to_string(),
                    };
                    lines.push(RenderedLine::new(
                        format!(
                            "$ {command} → exit {code}{}",
                            if *to_context {
                                ""
                            } else {
                                " (not sent to model)"
                            }
                        ),
                        LineKind::Tool,
                    ));
                    let total = output.lines().count();
                    for line in output.lines().take(SHELL_LINES) {
                        lines.push(RenderedLine::new(format!("  {line}"), LineKind::Dim));
                    }
                    if total > SHELL_LINES {
                        lines.push(RenderedLine::new(
                            format!("  [truncated: {total} lines]"),
                            LineKind::Dim,
                        ));
                    }
                }
                Block::Aborted => lines.push(RenderedLine::new("■ run aborted", LineKind::Dim)),
            }
        }
        lines
    }
}

/// Collapse to one line and cap the length, marking a cut with `…`.
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
