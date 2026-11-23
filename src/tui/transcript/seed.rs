//! Resume seeding: rebuild transcript blocks from persisted entries —
//! the same blocks the live event stream would have folded, so a
//! resumed session renders its past instead of a blank transcript.

use super::Transcript;
use crate::agent::session::{Entry, Message, Role};
use crate::events::AgentEvent;

impl Transcript {
    /// Seed from persisted entries (resume). Tool results attach to their
    /// calls with the ✓/✗ the disk knows; persisted thinking replays as
    /// one block before its text (the live stream's order); compaction
    /// entries render as a note.
    pub fn extend_from_entries(&mut self, entries: &[Entry]) {
        let results: Vec<(&str, &str, bool)> = entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Message {
                    message:
                        Message {
                            role: Role::ToolResult,
                            content,
                            tool_call_id: Some(id),
                            is_error,
                            ..
                        },
                    ..
                } => content
                    .as_deref()
                    .map(|output| (id.as_str(), output, *is_error)),
                _ => None,
            })
            .collect();

        for entry in entries {
            match entry {
                Entry::Message {
                    message:
                        Message {
                            role: Role::User,
                            content: Some(text),
                            ..
                        },
                    ..
                } => self.push_user(text),
                // A user entry without content (an abort marker) has no
                // block of its own here — the run's own events carry it.
                Entry::Message {
                    message:
                        Message {
                            role: Role::User, ..
                        },
                    ..
                } => {}
                Entry::Message {
                    message:
                        Message {
                            role: Role::Assistant,
                            content,
                            reasoning,
                            tool_calls,
                            ..
                        },
                    ..
                } => {
                    // Persisted thinking replays as one delta before the
                    // text — the same order the live stream produced.
                    if let Some(thinking) = reasoning
                        && !thinking.is_empty()
                    {
                        self.apply(&AgentEvent::Thinking {
                            delta: thinking.clone(),
                        });
                    }
                    if let Some(text) = content
                        && !text.is_empty()
                    {
                        self.apply(&AgentEvent::AssistantText {
                            delta: text.clone(),
                        });
                    }
                    for call in tool_calls {
                        self.apply(&AgentEvent::ToolCallStarted {
                            name: call.name.clone(),
                            id: call.id.clone(),
                            args: call.args.clone(),
                        });
                        if let Some((_, output, is_error)) =
                            results.iter().find(|(id, _, _)| *id == call.id)
                        {
                            self.apply(&AgentEvent::ToolCallFinished {
                                name: call.name.clone(),
                                id: call.id.clone(),
                                output: (*output).to_string(),
                                is_error: *is_error,
                            });
                        }
                    }
                }
                // Tool results attached to their calls above.
                Entry::Message {
                    message:
                        Message {
                            role: Role::ToolResult,
                            ..
                        },
                    ..
                } => {}
                Entry::Compaction { .. } => {
                    self.push_info("— earlier history compacted —");
                }
            }
        }
    }
}
