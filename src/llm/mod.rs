pub mod anthropic;
pub mod client;
pub mod stream;
pub mod types;

use std::fmt;

/// Wire protocol spoken by an endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiKind {
    #[default]
    OpenAiCompletions,
    AnthropicMessages,
}

impl ApiKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

impl fmt::Display for ApiKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an API key is attached to outgoing HTTP requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStyle {
    /// Bearer for OpenAI Completions; `x-api-key` for Anthropic Messages.
    #[default]
    Auto,
    Bearer,
    XApiKey,
}
