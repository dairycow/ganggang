//! The JSON a hook receives on stdin: one shape per event, with the
//! call/result payload of the loop moment it observes.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::HookEvent;
use crate::agent::session::ToolCall;
use crate::agent::tools::ToolResult;

const PAYLOAD_VERSION: u32 = 1;

#[derive(Serialize)]
pub(super) struct Payload {
    version: u32,
    event: &'static str,
    cwd: String,
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<ToolPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ResultPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) turns: Option<u32>,
}

#[derive(Serialize)]
struct ToolPayload {
    id: String,
    name: String,
    args: Value,
}

#[derive(Serialize)]
struct ResultPayload {
    output: String,
    is_error: bool,
}

impl Payload {
    pub(super) fn new(event: HookEvent, cwd: &Path, session_id: Option<&str>) -> Self {
        Self {
            version: PAYLOAD_VERSION,
            event: event.as_str(),
            cwd: cwd.display().to_string(),
            session_id: session_id.map(str::to_string),
            tool: None,
            result: None,
            turns: None,
        }
    }

    pub(super) fn with_tool(mut self, call: &ToolCall) -> Self {
        self.tool = Some(ToolPayload {
            id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
        });
        self
    }

    pub(super) fn with_result(mut self, result: &ToolResult) -> Self {
        self.result = Some(ResultPayload {
            output: result.output.clone(),
            is_error: result.is_error,
        });
        self
    }
}
