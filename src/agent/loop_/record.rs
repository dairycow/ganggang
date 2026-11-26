//! Entry append helpers for the loop (spec §13): the only code that grows
//! the session, so the persist-before-memory order lives in one place.

use crate::agent::Agent;
use crate::agent::session::{self, Entry, Message, Role, last_entry_id};
use crate::agent::tools::ToolResult;

impl Agent {
    /// Entries form a linear chain: each new one names the previous as
    /// parent. The entry is persisted BEFORE it joins the in-memory log —
    /// flush-per-append is the crash-safety contract (§13) — and session
    /// I/O errors abort the run (§17).
    pub(super) fn push_entry(&mut self, message: Message) -> anyhow::Result<()> {
        let parent = self.entries.last().map(last_entry_id);
        let entry = Entry::Message {
            id: session::new_entry_id(),
            parent_id: parent,
            timestamp: session::now_iso(),
            message,
        };
        if let Some(store) = &mut self.store {
            store.append(&entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Record the error that ended a run, so the session file says why
    /// the history stops where it does. Persisted before it joins memory
    /// like every entry. The caller treats it as best-effort: the run is
    /// already failing, and a session that cannot be written to cannot
    /// take a diagnostic either.
    pub(super) fn push_error(&mut self, error: &str) -> anyhow::Result<()> {
        let entry = Entry::Error {
            id: session::new_entry_id(),
            parent_id: self.entries.last().map(last_entry_id),
            timestamp: session::now_iso(),
            error: error.to_string(),
        };
        if let Some(store) = &mut self.store {
            store.append(&entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    pub(super) fn push_message(
        &mut self,
        role: Role,
        content: Option<String>,
    ) -> anyhow::Result<()> {
        self.push_entry(Message {
            role,
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
            is_error: false,
            reasoning: None,
        })
    }

    pub(super) fn push_tool_result(
        &mut self,
        call_id: &str,
        result: &ToolResult,
    ) -> anyhow::Result<()> {
        self.push_entry(Message {
            role: Role::ToolResult,
            content: Some(result.output.clone()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
            usage: None,
            is_error: result.is_error,
            reasoning: None,
        })
    }
}
