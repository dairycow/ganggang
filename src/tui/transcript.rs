//! The transcript model (spec §15): a pure fold of the event stream
//! into renderable blocks. Tool calls collapse to one line and expand on
//! request; warnings and the abort marker render dimmed. No terminal
//! dependency — rendering to lines is data, styling happens in the view.

use crate::events::AgentEvent;
use crate::tool_args::format_tool_args;

/// Max lines of tool output shown when a call is expanded.
const EXPAND_LINES: usize = 30;
/// Max lines of `!` shell output shown in the transcript.
const SHELL_LINES: usize = 20;
/// Max lines of thinking shown per block when thinking display is on.
const THINKING_LINES: usize = 30;
/// Max chars of a call's arguments on its one transcript line. Long
/// arguments wrap otherwise; the cut is marked with `…`.
const ARGS_CHARS: usize = 100;

#[derive(Debug, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    /// The last event was assistant text — the next delta continues it
    /// (the streaming shape). Any other event closes the block.
    assistant_open: bool,
    /// Same append rule for thinking deltas (they precede the text).
    thinking_open: bool,
    /// Ctrl+T: whether thinking blocks render at all. Off by default —
    /// thinking is stored either way; this only decides visibility.
    show_thinking: bool,
}

#[derive(Debug, PartialEq)]
enum Block {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    /// The model's thinking for one assistant turn (`reasoning_content`,
    /// M7.3). Folded from `Thinking` deltas; rendered only while
    /// `show_thinking` is on.
    Thinking {
        text: String,
    },
    ToolCall(ToolCall),
    Warning {
        message: String,
    },
    /// Command feedback: session info, skill lists, the /session list.
    /// Plain dim lines, no warning glyph — nothing went wrong.
    Info {
        text: String,
    },
    /// A command failed locally (bad index, unknown skill): worth a mark,
    /// but not the run-failure alarm shape.
    CommandError {
        message: String,
    },
    /// `!`/`!!` shell output: the command line, its exit code, and the
    /// capped output. `to_context: false` marks `!!` (shown, not sent).
    Shell {
        command: String,
        output: String,
        exit_code: Option<i32>,
        to_context: bool,
    },
    Aborted,
}

#[derive(Debug, PartialEq)]
struct ToolCall {
    name: String,
    id: String,
    args_display: String,
    output: Option<String>,
    is_error: bool,
    expanded: bool,
}

impl Transcript {
    pub fn push_user(&mut self, text: &str) {
        self.blocks.push(Block::User {
            text: text.to_string(),
        });
        self.assistant_open = false;
        self.thinking_open = false;
    }

    /// Multi-line command feedback (the hooks listing, session info).
    /// Blank lines are kept — they group the listing.
    pub fn push_info(&mut self, text: &str) {
        self.blocks.push(Block::Info {
            text: text.to_string(),
        });
        self.assistant_open = false;
        self.thinking_open = false;
    }

    /// A command failed locally — distinct from run failures.
    pub fn push_command_error(&mut self, message: &str) {
        self.blocks.push(Block::CommandError {
            message: message.to_string(),
        });
        self.assistant_open = false;
        self.thinking_open = false;
    }

    /// One `!`/`!!` shell execution for the human to read.
    pub fn push_shell(
        &mut self,
        command: &str,
        output: &str,
        exit_code: Option<i32>,
        to_context: bool,
    ) {
        self.blocks.push(Block::Shell {
            command: command.to_string(),
            output: output.to_string(),
            exit_code,
            to_context,
        });
        self.assistant_open = false;
        self.thinking_open = false;
    }

    /// Nothing to show yet — no block of its own, not even a note. This is
    /// what the view's welcome block answers, and nothing else: a scroll
    /// position or a draft does not make a session less empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The last assistant answer, for Ctrl+X: the text of the most
    /// recent assistant block, if the conversation has one.
    pub fn last_answer(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|b| match b {
            Block::Assistant { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Fold one agent event into the transcript.
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AssistantText { delta } => {
                if delta.is_empty() {
                    return;
                }
                if self.assistant_open {
                    match self.blocks.last_mut() {
                        Some(Block::Assistant { text }) => text.push_str(delta),
                        _ => self.blocks.push(Block::Assistant {
                            text: delta.clone(),
                        }),
                    }
                } else {
                    self.blocks.push(Block::Assistant {
                        text: delta.clone(),
                    });
                }
                self.assistant_open = true;
                self.thinking_open = false;
            }
            AgentEvent::Thinking { delta } => {
                if delta.is_empty() {
                    return;
                }
                if self.thinking_open {
                    match self.blocks.last_mut() {
                        Some(Block::Thinking { text }) => text.push_str(delta),
                        _ => self.blocks.push(Block::Thinking {
                            text: delta.clone(),
                        }),
                    }
                } else {
                    self.blocks.push(Block::Thinking {
                        text: delta.clone(),
                    });
                }
                self.thinking_open = true;
                self.assistant_open = false;
            }
            AgentEvent::ToolCallStarted { name, id, args } => {
                self.blocks.push(Block::ToolCall(ToolCall {
                    name: name.clone(),
                    id: id.clone(),
                    args_display: format_tool_args(name, args, ARGS_CHARS),
                    output: None,
                    is_error: false,
                    expanded: false,
                }));
                self.assistant_open = false;
                self.thinking_open = false;
            }
            AgentEvent::ToolCallFinished {
                id,
                output,
                is_error,
                ..
            } => {
                // Calls dispatch sequentially; the finished one is the
                // last unfinished call with this id (search from the end).
                for block in self.blocks.iter_mut().rev() {
                    if let Block::ToolCall(call) = block
                        && call.id == *id
                        && call.output.is_none()
                    {
                        call.output = Some(output.clone());
                        call.is_error = *is_error;
                        break;
                    }
                }
                self.assistant_open = false;
                self.thinking_open = false;
            }
            AgentEvent::Warned { message } => {
                self.blocks.push(Block::Warning {
                    message: message.clone(),
                });
                self.assistant_open = false;
                self.thinking_open = false;
            }
            AgentEvent::RunAborted => {
                self.blocks.push(Block::Aborted);
                self.assistant_open = false;
                self.thinking_open = false;
            }
            // Run/turn markers carry no transcript content. `Subtask` is
            // ignored here too: the nested run's rows are a follow-up, and
            // forwarding its `TurnEnded` into `State::apply` would corrupt
            // the parent's context footer with the child's usage.
            AgentEvent::RunStarted
            | AgentEvent::TurnStarted
            | AgentEvent::TurnEnded { .. }
            | AgentEvent::RunEnded { .. }
            | AgentEvent::Subtask { .. } => {
                self.assistant_open = false;
                self.thinking_open = false;
            }
        }
    }

    /// Ctrl+T: flip whether thinking blocks render. Stored thinking is
    /// never lost — the flag only decides visibility.
    pub fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
    }

    /// Ctrl+O: expand the most recent collapsed call; when every call is
    /// already expanded, collapse them all. One key, one predictable
    /// direction at a time.
    pub fn toggle_expand(&mut self) {
        let last_collapsed = self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::ToolCall(c) if !c.expanded => Some(c),
            _ => None,
        });
        match last_collapsed {
            Some(call) => call.expanded = true,
            None => {
                for block in &mut self.blocks {
                    if let Block::ToolCall(call) = block {
                        call.expanded = false;
                    }
                }
            }
        }
    }
}

mod markdown;
mod render;
mod seed;
pub use render::{Align, LineKind, Mark, RenderedLine, TableRow};

#[cfg(test)]
mod tests;
