//! Session entries: the persisted, replayable history the agent owns.
//!
//! The wire types in [`crate::llm::types`] mirror an external protocol and
//! stay loose. These domain types are the internal source of truth and stay
//! strict: roles are an enum, tool arguments are parsed JSON. All conversion
//! happens in [`to_wire`] / [`from_response`] (M2.3) so provider-shape
//! knowledge never spreads past this module.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod io;

use crate::llm::types::{ChatMessage, ChatResponse, ToolCall as WireToolCall};

/// One entry in a session file. `parentId` chains entries linearly today;
/// the field exists from day one so tree navigation can be added later
/// without a format migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        message: Message,
    },
    /// Appended by compaction (M6). The file keeps every raw entry; replay
    /// starts at `firstKeptId` with `summary` in front.
    Compaction {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        #[serde(rename = "firstKeptId")]
        first_kept_id: String,
    },
}

/// Domain message: strict where the wire is loose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    /// Thinking the model did before this message (`reasoning_content`,
    /// M7.3). Persisted when non-empty and replayed according to
    /// [`ReplayReasoning`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Domain role. The file format keeps `tool_result` distinct from the
/// provider's `"tool"` role; the mapping lives in `to_wire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    ToolResult,
}

/// A tool call the assistant made, with arguments already parsed.
///
/// `args_error` is set when the provider's argument string was not valid
/// JSON: `args` is then an empty object and dispatch refuses the call.
/// Persisted so a replayed session explains itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_error: Option<String>,
}

/// How assistant thinking (`reasoning_content`) is replayed on subsequent
/// requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplayReasoning {
    /// Never replay `reasoning_content` (default for non-reasoning models).
    #[default]
    Never,
    /// Replay `reasoning_content` only for assistant turns since the last
    /// `Role::User` message (keeps KV prefix append-only within a tool-use
    /// turn, drops older thinking at user/compaction boundaries).
    Turn,
    /// Replay `reasoning_content` for all assistant turns in the active window.
    All,
}

impl ReplayReasoning {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Never => "no",
            Self::Turn => "turn",
            Self::All => "yes",
        }
    }
}

impl<'de> serde::Deserialize<'de> for ReplayReasoning {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ReplayReasoning;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a boolean or one of \"never\", \"turn\", \"all\"")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(if v {
                    ReplayReasoning::All
                } else {
                    ReplayReasoning::Never
                })
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "never" | "none" | "off" => Ok(ReplayReasoning::Never),
                    "turn" => Ok(ReplayReasoning::Turn),
                    "all" | "always" => Ok(ReplayReasoning::All),
                    other => Err(E::custom(format!(
                        "invalid replay_reasoning {other:?}; expected boolean or \"never\", \"turn\", \"all\""
                    ))),
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Token usage recorded on assistant entries; the TUI footer totals these.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

/// Percentage of output tokens spent on thinking, if known from usage or
/// character ratio.
pub fn thinking_pct(message: &Message) -> Option<u8> {
    if let Some(u) = &message.usage
        && let Some(r) = u.reasoning_tokens.filter(|&r| r > 0)
        && u.completion_tokens > 0
    {
        return Some(((r as u64 * 100) / u.completion_tokens as u64).min(100) as u8);
    }
    let reason = message.reasoning.as_deref().filter(|r| !r.is_empty())?;
    let reason_chars = reason.chars().count();
    let out_chars = message.content.as_deref().map_or(0, |c| c.chars().count())
        + message
            .tool_calls
            .iter()
            .map(|c| c.args.to_string().chars().count())
            .sum::<usize>();
    let total = reason_chars + out_chars;
    if total == 0 {
        return None;
    }
    Some(((reason_chars * 100) / total).min(100) as u8)
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Entry ids are the first 8 hex chars of a uuid v4: short enough to read,
/// unique enough to never collide in a human-scale session.
pub fn new_entry_id() -> String {
    // `simple()` always renders the full 32 hex digits, so the slice is safe.
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// ISO-8601 timestamp for session entries. Formatting a `now_utc` value
/// cannot fail (the error type only exists for out-of-range constructed
/// datetimes); the empty string is the never-taken fallback.
pub fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// The id of an entry, whatever variant it is — the parent of whatever
/// comes next in the linear chain.
pub(crate) fn last_entry_id(entry: &Entry) -> String {
    let (Entry::Message { id, .. } | Entry::Compaction { id, .. }) = entry;
    id.clone()
}

/// Replay view of a session: entries → wire messages for the next request.
///
/// The LAST compaction governs (§14): its summary leads as a user note,
/// then the entries from `firstKeptId` onward replay in full. Raw entries
/// before the boundary stay in the file — only the replay view shrinks.
/// `firstKeptId` in gg-written files names a user message (or an assistant
/// boundary in single-prompt multi-turn runs); if a hand-edited file points
/// mid-span instead, replay snaps forward to the next user or assistant
/// message rather than orphaning a tool result from its call.
#[cfg_attr(not(test), allow(dead_code))]
pub fn to_wire(entries: &[Entry]) -> Vec<ChatMessage> {
    to_wire_with(entries, ReplayReasoning::Never)
}

pub fn to_wire_with(entries: &[Entry], replay_reasoning: ReplayReasoning) -> Vec<ChatMessage> {
    let plain = |slice: &[Entry]| {
        let last_user_idx = slice.iter().rposition(
            |e| matches!(e, Entry::Message { message, .. } if message.role == Role::User),
        );
        slice
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| match entry {
                Entry::Message { message, .. } => {
                    let include_reasoning = match replay_reasoning {
                        ReplayReasoning::Never => false,
                        ReplayReasoning::All => true,
                        ReplayReasoning::Turn => last_user_idx.is_none_or(|u| i > u),
                    };
                    Some(message_to_wire(
                        message,
                        replay_reasoning,
                        include_reasoning,
                    ))
                }
                Entry::Compaction { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    let Some(idx) = entries
        .iter()
        .rposition(|e| matches!(e, Entry::Compaction { .. }))
    else {
        return plain(entries);
    };
    let Entry::Compaction {
        summary,
        first_kept_id,
        ..
    } = &entries[idx]
    else {
        return plain(entries);
    };
    let Some(start) = entries
        .iter()
        .position(|e| last_entry_id(e) == *first_kept_id)
    else {
        // The boundary no longer resolves (hand-edited file): replay
        // everything rather than silently losing the tail.
        return plain(entries);
    };
    let kept = entries[start..]
        .iter()
        .position(|e| {
            matches!(
                e,
                Entry::Message { message, .. } if matches!(message.role, Role::User | Role::Assistant)
            )
        })
        .map(|offset| start + offset);
    let note_content = match kept {
        Some(offset)
            if matches!(
                &entries[offset],
                Entry::Message { message, .. } if message.role == Role::Assistant
            ) =>
        {
            let pinned = entries[..offset].iter().rev().find_map(|e| match e {
                Entry::Message { message, .. }
                    if message.role == Role::User
                        && message
                            .content
                            .as_deref()
                            .is_some_and(|c| !c.is_empty() && c != "[aborted by user]") =>
                {
                    message.content.as_deref()
                }
                _ => None,
            });
            match pinned {
                Some(req) => {
                    let req_capped = if req.chars().count() > 16_384 {
                        let cut: String = req.chars().take(16_384).collect();
                        format!("{cut}\n[truncated]")
                    } else {
                        req.to_string()
                    };
                    format!(
                        "[original user request]\n{req_capped}\n\n[conversation before this point was compacted]\n{summary}"
                    )
                }
                None => format!("[conversation before this point was compacted]\n{summary}"),
            }
        }
        _ => format!("[conversation before this point was compacted]\n{summary}"),
    };
    let note = ChatMessage {
        role: "user".to_string(),
        content: Some(note_content),
        ..ChatMessage::default()
    };
    match kept {
        Some(offset) => {
            let mut out = Vec::with_capacity(entries.len() - offset + 1);
            out.push(note);
            out.extend(plain(&entries[offset..]));
            out
        }
        // No user or assistant message at or after the boundary: the note
        // alone carries the context forward.
        None => vec![note],
    }
}

fn message_to_wire(
    m: &Message,
    replay_reasoning: ReplayReasoning,
    include_reasoning: bool,
) -> ChatMessage {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::ToolResult => "tool",
    };
    let reasoning_content = if include_reasoning && m.role == Role::Assistant {
        match replay_reasoning {
            ReplayReasoning::Never => None,
            ReplayReasoning::Turn => m.reasoning.clone().filter(|r| !r.is_empty()),
            ReplayReasoning::All => Some(m.reasoning.clone().unwrap_or_default()),
        }
    } else {
        None
    };
    ChatMessage {
        role: role.to_string(),
        content: m.content.clone(),
        tool_calls: m.tool_calls.iter().map(tool_call_to_wire).collect(),
        tool_call_id: m.tool_call_id.clone(),
        reasoning_content,
        reasoning: None,
        is_error: m.is_error,
    }
}

fn tool_call_to_wire(c: &ToolCall) -> WireToolCall {
    WireToolCall::function(
        c.id.clone(),
        c.name.clone(),
        // Serializing a `Value` that came from `serde_json` cannot fail;
        // the empty string is the never-taken fallback.
        serde_json::to_string(&c.args).unwrap_or_default(),
    )
}

/// Record a provider response as a domain message.
///
/// Returns the message plus warnings for anything degraded but survivable
/// (unparseable tool arguments). An `Err` means the response was unusable
/// and the run cannot continue.
pub fn from_response(resp: ChatResponse) -> anyhow::Result<(Message, Vec<String>)> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .context("provider returned no choices")?;
    let wire = choice.message;

    let mut warnings = Vec::new();
    let tool_calls = wire
        .tool_calls
        .iter()
        .map(|c| {
            let (args, args_error) = parse_arguments(&c.function.arguments);
            if let Some(detail) = &args_error {
                warnings.push(format!("tool arguments were not valid JSON: {detail}"));
            }
            ToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                args,
                args_error,
            }
        })
        .collect();

    let message = Message {
        role: Role::Assistant,
        content: wire.content,
        tool_calls,
        tool_call_id: None,
        usage: resp.usage.map(|u| {
            let reasoning_tokens = u.effective_reasoning_tokens();
            Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                reasoning_tokens,
            }
        }),
        is_error: false,
        reasoning: wire
            .reasoning_content
            .or(wire.reasoning)
            .filter(|r| !r.is_empty()),
    };
    Ok((message, warnings))
}

/// Parse the JSON-in-a-string tool arguments once, at the boundary.
/// An empty string is the provider's way of saying "no arguments". Malformed
/// JSON yields empty args plus the parse detail; dispatch turns that into a
/// retryable error result for the model instead of a dead run.
fn parse_arguments(raw: &str) -> (Value, Option<String>) {
    if raw.trim().is_empty() {
        return (Value::Object(serde_json::Map::new()), None);
    }
    match serde_json::from_str(raw) {
        Ok(v) => (v, None),
        Err(e) => {
            // The excerpt is echoed into events and the transcript; cap it.
            let excerpt: String = raw.chars().take(200).collect();
            (
                Value::Object(serde_json::Map::new()),
                Some(format!("{e}: `{excerpt}`")),
            )
        }
    }
}

#[cfg(test)]
mod tests;
