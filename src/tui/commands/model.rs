//! `/model`: switch the active endpoint; with no argument, point the
//! user at the list above the composer, which is where the models are.

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
            state.push_info("pick a model from the list above the composer: ↑↓ then Enter or Tab");
        }
        Some(choice) => {
            // The list's own rows are what a discovered model is known by.
            let discovered: Vec<(String, String)> = state
                .models_arc()
                .iter()
                .map(|m| (m.provider.clone(), m.model.clone()))
                .collect();
            let mut endpoint = config.resolve_switch_with_discovered(&choice, &discovered)?;
            if endpoint.provider == active_endpoint.provider
                && endpoint.model == active_endpoint.model
            {
                state.push_info(&format!("already on {}", endpoint.label()));
                return Ok(());
            }
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
