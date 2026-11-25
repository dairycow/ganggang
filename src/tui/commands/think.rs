//! `/think`: show or set thinking effort and token budget on the active
//! OpenAI-compatible endpoint.

use crate::llm::ApiKind;
use crate::llm::client::LlmClient;

use super::CmdCtx;

const USAGE: &str = "usage: /think [effort|off|default] [budget]";

/// Parse `/think` arguments against the active endpoint's current
/// settings: `off`/`none` sets effort to `"none"` and clears the budget;
/// `default`/`clear` clears both so the provider uses its default; a bare
/// number sets the budget (0 clears it); a bare word sets the effort; and
/// `effort budget` sets both.
pub(super) fn parse_think_args(
    raw: &str,
    current_effort: Option<&str>,
    current_budget: Option<u32>,
) -> anyhow::Result<(Option<String>, Option<u32>)> {
    let tokens: Vec<&str> = raw.split_ascii_whitespace().collect();
    let is_off = |t: &str| t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("none");
    let is_default = |t: &str| t.eq_ignore_ascii_case("default") || t.eq_ignore_ascii_case("clear");
    match tokens.as_slice() {
        [] => anyhow::bail!(USAGE),
        [only] if is_off(only) => Ok((Some("none".to_string()), None)),
        [only] if is_default(only) => Ok((None, None)),
        [only] => match only.parse::<u32>() {
            Ok(0) => Ok((current_effort.map(str::to_string), None)),
            Ok(n) => Ok((current_effort.map(str::to_string), Some(n))),
            Err(_) => Ok((Some(only.to_string()), current_budget)),
        },
        [effort, budget]
            if !is_off(effort) && !is_default(effort) && effort.parse::<u32>().is_err() =>
        {
            let budget = match budget.parse::<u32>() {
                Ok(0) => None,
                Ok(n) => Some(n),
                Err(_) => anyhow::bail!(USAGE),
            };
            Ok((Some(effort.to_string()), budget))
        }
        _ => anyhow::bail!(USAGE),
    }
}

pub(super) async fn handle_think(arg: Option<String>, ctx: CmdCtx<'_>) -> anyhow::Result<()> {
    let CmdCtx {
        state,
        input_tx,
        active_endpoint,
        ..
    } = ctx;

    if active_endpoint.api == ApiKind::AnthropicMessages {
        match arg {
            None => state.push_info("thinking: not supported on anthropic-messages endpoints"),
            Some(_) => {
                state.push_warning("/think is only supported on openai-completions endpoints");
            }
        }
        return Ok(());
    }

    match arg {
        None => {
            let summary = thinking_summary(
                active_endpoint.reasoning_effort.as_deref(),
                active_endpoint.reasoning_budget_tokens,
            );
            state.push_info(&format!(
                "thinking: {summary}\n/think [effort|off|default] [budget] to change"
            ));
        }
        Some(arg) => {
            let (effort, budget) = parse_think_args(
                &arg,
                active_endpoint.reasoning_effort.as_deref(),
                active_endpoint.reasoning_budget_tokens,
            )?;
            let mut endpoint = active_endpoint.clone();
            endpoint.reasoning_effort = effort;
            endpoint.reasoning_budget_tokens = budget;
            let _ = LlmClient::from_endpoint(&endpoint)?;
            input_tx
                .send(super::super::AgentInput::SwitchEndpoint(endpoint.clone()))
                .await?;
            *active_endpoint = endpoint.clone();
            state.set_thinking(
                endpoint.reasoning_effort.as_deref(),
                endpoint.reasoning_budget_tokens,
            );
            let summary = thinking_summary(
                endpoint.reasoning_effort.as_deref(),
                endpoint.reasoning_budget_tokens,
            );
            state.push_info(&format!("thinking: {summary}"));
        }
    }
    Ok(())
}

/// The active endpoint's thinking configuration in one phrase, for the
/// `/think` feedback line: the effort, the budget, or the provider
/// default when neither is set.
fn thinking_summary(effort: Option<&str>, budget: Option<u32>) -> String {
    match (effort, budget) {
        (Some(effort), Some(tokens)) => format!("effort {effort}, budget {tokens} tokens"),
        (Some(effort), None) => format!("effort {effort}"),
        (None, Some(tokens)) => format!("budget {tokens} tokens"),
        (None, None) => "provider default".to_string(),
    }
}
