//! Resume-time repairs: a torn trailing newline, and assistant tool calls
//! whose results never landed.

use std::io::Write;
use std::path::Path;

use anyhow::Context;

use super::super::{Entry, Message, Role};
use super::SessionStore;

/// A crash between a line's text and its newline leaves the file without a
/// trailing `\n`. Append it back through append-mode (which writes at the
/// end regardless of any stored offset) so the torn entry stays its own
/// line instead of concatenating with the next append.
pub(crate) fn repair_missing_trailing_newline(path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if matches!(bytes.last(), None | Some(b'\n')) {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for repair", path.display()))?;
    file.write_all(b"\n")
        .and_then(|()| file.flush())
        .with_context(|| format!("repairing {}", path.display()))
}

/// Resume repair (§8: a tool_call is never replayed without its result):
/// assistant calls whose results never landed — a kill -9 or Ctrl+C
/// mid-batch — get synthesized `is_error` results, appended through the
/// store so disk and memory agree. Returns how many were synthesized.
pub fn repair_dangling_tool_calls(
    entries: &mut Vec<Entry>,
    store: &mut SessionStore,
) -> anyhow::Result<usize> {
    let answered: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message { message, .. } if message.role == Role::ToolResult => {
                message.tool_call_id.clone()
            }
            _ => None,
        })
        .collect();
    let dangling: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message { message, .. } if message.role == Role::Assistant => {
                Some(message.tool_calls.iter().map(|c| c.id.clone()))
            }
            _ => None,
        })
        .flatten()
        .filter(|id| !answered.contains(id))
        .collect();

    let repaired = dangling.len();
    for id in dangling {
        // One Entry, one id: appended to disk, then recorded in memory.
        let entry = Entry::Message {
            id: super::super::new_entry_id(),
            parent_id: entries.last().map(super::super::last_entry_id),
            timestamp: super::super::now_iso(),
            message: Message {
                role: Role::ToolResult,
                content: Some("aborted before execution".into()),
                tool_calls: Vec::new(),
                tool_call_id: Some(id),
                usage: None,
                is_error: true,
                reasoning: None,
            },
        };
        store.append(&entry)?;
        entries.push(entry);
    }
    Ok(repaired)
}
