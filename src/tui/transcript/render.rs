//! Rendering the transcript model: blocks become styled lines, tool
//! calls collapse or expand, everything stays data — no terminal here.
//!
//! A line carries three things: its text, what the line IS (`kind`), and
//! the spans inside it that read differently (`marks`) — the `you │ `
//! prefix, a tool call's arguments, a status glyph. What a line is stays
//! here; what a colour means lives in
//! [`theme`](super::super::theme), so a palette change never reaches into
//! the fold.

use super::markdown;
use super::{Block, EXPAND_LINES, SHELL_LINES, THINKING_LINES};

/// How a rendered line is styled. Every kind has exactly one job in the
/// view: [`Palette::line`](super::super::theme::Palette::line) turns it
/// into a style, so the difference between a warning and a note is a
/// model distinction (which kind this line is) and never a colour
/// guessed at draw time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    /// Inside a fenced code block (M7.6): a band of its own, no markdown.
    Code,
    /// A laid-out markdown table row (grid or its narrow fallback).
    Table,
    /// A tool call line, or a `!`/`!!` shell command line.
    Tool,
    /// A tool call that failed. The line reads like a call; the glyph in
    /// its marks says what went wrong.
    ToolError,
    /// Output under an expanded call or a shell line: set back from the
    /// conversation, on a raised band.
    Output,
    /// Command feedback — a listing, session info. Nothing went wrong.
    Info,
    /// A warning, including a run's failure tail.
    Warning,
    /// A command that failed locally.
    CommandError,
    /// `■ run aborted`: the mark of a run the user stopped.
    Aborted,
    /// The model's thinking, shown on request (Ctrl+T).
    Thinking,
}

/// A span of a line that reads differently from the line around it. The
/// ranges are what the model knows about its own text; the colour they
/// mean is the theme's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Markdown emphasis: heavier than its line, same colour.
    Bold,
    /// The crest: `you │ `, and the abort marker.
    Crest,
    /// A call's marker and name (`▸ read`, `$ ls`).
    Name,
    /// A call's arguments, and a command line's tail: secondary.
    Args,
    /// `✓`: a call that worked.
    Good,
    /// `✗`: a call that did not.
    Bad,
}

/// Column alignment from a pipe table's delimiter row (`:--` left,
/// `--:` right, `:--:` centre; the default is left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

/// One row of a markdown pipe table before layout. The model carries the
/// cells; the view lays them out at the terminal's width
/// ([`table`](super::super::table)), so nothing here needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<String>,
    pub aligns: Vec<Align>,
    pub header: bool,
}

/// One rendered line: text plus what it is, the spans that read
/// differently, and — for markdown — nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    pub text: String,
    pub kind: LineKind,
    /// Sorted, disjoint `(start, end, mark)` char ranges into `text`.
    /// The builders below produce them left to right, and
    /// [`wrap_line`](super::super::line::wrap_line) clips them to rows,
    /// so the view can walk a line's chars once.
    pub marks: Vec<(usize, usize, Mark)>,
    /// A table row awaiting layout; `text` stays empty until the view
    /// turns the run of table rows into lines at a known width.
    pub table: Option<TableRow>,
}

impl RenderedLine {
    pub fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
            marks: Vec::new(),
            table: None,
        }
    }

    /// Attach the spans that read differently (markdown emphasis, a
    /// prefix, a status glyph).
    pub fn with_marks(mut self, marks: Vec<(usize, usize, Mark)>) -> Self {
        self.marks = marks;
        self
    }

    /// Attach a table row's structure, to be laid out by the view.
    pub fn with_table(mut self, row: TableRow) -> Self {
        self.table = Some(row);
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
                        lines.push(user_line(line));
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
                        lines.push(RenderedLine::new(format!("~ {line}"), LineKind::Thinking));
                    }
                    if total > THINKING_LINES {
                        lines.push(RenderedLine::new(
                            format!("~ [truncated: {total} lines]"),
                            LineKind::Thinking,
                        ));
                    }
                }
                Block::Thinking { .. } => {}
                Block::ToolCall(call) => {
                    lines.push(tool_line(call));
                    if call.expanded
                        && let Some(output) = &call.output
                    {
                        lines.extend(capped_output(output, EXPAND_LINES));
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
                        lines.push(RenderedLine::new(text, LineKind::Warning));
                    }
                }
                Block::Info { text } => {
                    for line in text.split('\n') {
                        lines.push(RenderedLine::new(line, LineKind::Info));
                    }
                }
                Block::CommandError { message } => {
                    let text = format!("✗ {message}");
                    lines.push(
                        RenderedLine::new(text, LineKind::CommandError).with_marks(vec![(
                            0,
                            1,
                            Mark::Bad,
                        )]),
                    );
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
                    let head = format!("$ {command}");
                    let tail = format!(
                        " → exit {code}{}",
                        if *to_context {
                            ""
                        } else {
                            " (not sent to model)"
                        }
                    );
                    let head_len = head.chars().count();
                    lines.push(
                        RenderedLine::new(format!("{head}{tail}"), LineKind::Tool).with_marks(
                            vec![
                                (0, head_len, Mark::Name),
                                (head_len, head_len + tail.chars().count(), Mark::Args),
                            ],
                        ),
                    );
                    lines.extend(capped_output(output, SHELL_LINES));
                }
                Block::Aborted => {
                    let text = "■ run aborted";
                    let end = text.chars().count();
                    lines.push(RenderedLine::new(text, LineKind::Aborted).with_marks(vec![(
                        0,
                        end,
                        Mark::Crest,
                    )]));
                }
            }
        }
        lines
    }
}

/// A user line: the `you │ ` prefix is gg's own mark, the prompt is the
/// user's text.
fn user_line(text: &str) -> RenderedLine {
    let prefix = "you │ ";
    let mark = (0, prefix.chars().count(), Mark::Crest);
    RenderedLine::new(format!("{prefix}{text}"), LineKind::User).with_marks(vec![mark])
}

/// A collapsed (or expanded, by its marker) tool call: marker, name,
/// arguments, and a status glyph once the call has finished.
fn tool_line(call: &super::ToolCall) -> RenderedLine {
    let head = format!("{} {}", if call.expanded { "▾" } else { "▸" }, call.name);
    let args = format!(" {}", call.args_display);
    let status = match (&call.output, call.is_error) {
        (None, _) => "",
        (Some(_), true) => " ✗",
        (Some(_), false) => " ✓",
    };
    let head_len = head.chars().count();
    let args_end = head_len + args.chars().count();
    let total = args_end + status.chars().count();
    let mut marks = vec![(0, head_len, Mark::Name), (head_len, args_end, Mark::Args)];
    if !status.is_empty() {
        // The glyph is the line's last char; its leading space belongs to
        // the arguments' span.
        let glyph = (
            total - 1,
            total,
            if call.is_error { Mark::Bad } else { Mark::Good },
        );
        marks.push(glyph);
    }
    let kind = if call.is_error {
        LineKind::ToolError
    } else {
        LineKind::Tool
    };
    RenderedLine::new(format!("{head}{args}{status}"), kind).with_marks(marks)
}

/// Output under an expanded call or a shell line: indented, capped, with
/// the same honest truncation marker.
fn capped_output(output: &str, cap: usize) -> Vec<RenderedLine> {
    let total = output.lines().count();
    let mut lines: Vec<RenderedLine> = output
        .lines()
        .take(cap)
        .map(|line| RenderedLine::new(format!("  {line}"), LineKind::Output))
        .collect();
    if total > cap {
        lines.push(RenderedLine::new(
            format!("  [truncated: {total} lines]"),
            LineKind::Output,
        ));
    }
    lines
}
