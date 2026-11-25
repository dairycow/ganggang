//! `/model`: list configured/discovered models or switch the active endpoint.

use super::CmdCtx;

pub(super) async fn handle_model(choice: Option<String>, ctx: CmdCtx<'_>) -> anyhow::Result<()> {
    let CmdCtx {
        state,
        input_tx,
        config,
        active_endpoint,
        ..
    } = ctx;

    match choice {
        None => {
            let discovered =
                crate::llm::discovery::discover_local_providers(&config.providers).await;
            let rows =
                crate::config::providers::catalog_with_discovered(&config.providers, &discovered);
            state.pending_models = rows
                .iter()
                .map(|r| (r.provider.clone(), r.model.clone()))
                .collect();
            let mut listing = String::from("models:");
            for (i, row) in rows.iter().enumerate() {
                let active = if row.provider == active_endpoint.provider
                    && row.model == active_endpoint.model
                {
                    " [active]"
                } else {
                    ""
                };
                let proto = match row.api {
                    crate::llm::ApiKind::OpenAiCompletions => "",
                    crate::llm::ApiKind::AnthropicMessages => " · anthropic",
                };
                let status_note = row
                    .status
                    .as_deref()
                    .map(|s| format!(" · {s}"))
                    .unwrap_or_default();
                let key_note = if row.needs_key && !row.has_key {
                    " [no key]"
                } else {
                    ""
                };
                listing.push_str(&format!(
                    "\n  {}) {}/{} (ctx {}k{}{}{}){}",
                    i + 1,
                    row.provider,
                    row.model,
                    row.context_window / 1000,
                    status_note,
                    proto,
                    key_note,
                    active,
                ));
            }
            listing.push_str("\n/model <n|provider|provider/model> to switch");
            state.push_info(&listing);
        }
        Some(choice) => {
            let target = match choice.parse::<usize>() {
                Ok(n) if n >= 1 => {
                    let (p, m) = state
                        .pending_models
                        .get(n - 1)
                        .ok_or_else(|| anyhow::anyhow!("{n} is not in the last /model listing"))?;
                    format!("{p}/{m}")
                }
                _ => choice,
            };
            let mut endpoint =
                config.resolve_switch_with_discovered(&target, &state.pending_models)?;
            endpoint.require_auth()?;
            crate::llm::health::maybe_probe_local_health(&mut endpoint).await;
            let _ = crate::llm::client::LlmClient::from_endpoint(&endpoint)?;
            input_tx
                .send(super::super::AgentInput::SwitchEndpoint(endpoint.clone()))
                .await?;
            *active_endpoint = endpoint.clone();
            state.set_model(&endpoint.label(), endpoint.context_window);
            state.set_thinking(
                endpoint.reasoning_effort.as_deref(),
                endpoint.reasoning_budget_tokens,
            );
            state.push_info(&format!(
                "switched to {} ({}, ctx {}k)",
                endpoint.label(),
                endpoint.api,
                endpoint.context_window / 1000
            ));
        }
    }
    Ok(())
}
