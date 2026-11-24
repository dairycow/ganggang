//! The wire shape of one SSE chunk from an OpenAI-compatible stream: what
//! the assembler folds. Permissive on purpose — providers add fields, omit
//! others, and send `delta: {}` on the chunk that only carries a finish
//! reason (`#[serde(default)]` everywhere, so none of it is an error).

use serde::Deserialize;

use super::super::types::{Timings, Usage};

#[derive(Debug, Deserialize)]
pub(super) struct StreamChunk {
    #[serde(default)]
    pub(super) choices: Vec<StreamChoice>,
    #[serde(default)]
    pub(super) usage: Option<Usage>,
    #[serde(default)]
    pub(super) timings: Option<Timings>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamChoice {
    #[serde(default)]
    pub(super) index: u32,
    #[serde(default)]
    pub(super) delta: StreamDelta,
    #[serde(default)]
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct StreamDelta {
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) reasoning_content: Option<String>,
    #[serde(default)]
    pub(super) reasoning: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Vec<StreamToolCall>,
}
#[derive(Debug, Deserialize)]
pub(super) struct StreamToolCall {
    pub(super) index: u32,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) function: Option<StreamFunction>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct StreamFunction {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) arguments: Option<String>,
}
