//! Background local model discovery for the `/model` popup: probes the
//! `/v1/models` endpoints of local OpenAI-compatible servers and reports
//! the refreshed rows to the UI, so the popup and the `/model` listing
//! never wait on a slow or down server.

use std::collections::BTreeMap;

use tokio::sync::mpsc;

use crate::config::ProviderConfig;

use super::agent_task::UiEvent;
use super::suggest::ModelChoice;

/// Spawns a task that probes the local providers' `/v1/models` endpoints
/// and sends the refreshed rows as [`UiEvent::ModelsDiscovered`]. It runs
/// off the render loop with short timeouts, so down local servers never
/// block startup or `/reload`; if the UI has exited, the send fails and
/// the task ends quietly.
pub(super) fn spawn_model_discovery(
    providers: BTreeMap<String, ProviderConfig>,
    ui_tx: mpsc::Sender<UiEvent>,
) {
    tokio::spawn(async move {
        let discovered = crate::llm::discovery::discover_local_providers(&providers).await;
        let rows = crate::config::providers::catalog_with_discovered(&providers, &discovered);
        let models: Vec<ModelChoice> = rows.iter().map(Into::into).collect();
        let _ = ui_tx.send(UiEvent::ModelsDiscovered(models)).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// The late discovery event replaces the list's rows: `/model` lists
    /// exactly the discovered model.
    #[tokio::test]
    async fn apply_models_discovered_refreshes_the_model_list() {
        let (mut input_tx, _input_rx) = mpsc::channel::<super::super::agent_task::AgentInput>(1);
        let mut state = super::super::view::State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(Vec::new()),
            super::super::history::History::default(),
        );
        state.editor.set_text("/model");
        state.sync_picker();
        let event = UiEvent::ModelsDiscovered(vec![ModelChoice {
            provider: "llama-cpp".into(),
            model: "local-m".into(),
            context_window: 16_384,
            status: Some("loaded".into()),
            needs_key: false,
            has_key: true,
        }]);
        assert!(
            super::super::agent_task::apply(event, &mut state, &mut input_tx).await,
            "the event folds into state without ending the TUI"
        );
        let rows = state.picker_matches();
        let labels: Vec<&str> = rows.iter().filter_map(|row| row.label()).collect();
        assert_eq!(labels, vec!["llama-cpp/local-m"], "the rows are replaced");
    }

    /// The spawned probe hits a fake local server's `/v1/models` and the
    /// discovered id arrives as [`UiEvent::ModelsDiscovered`].
    #[tokio::test]
    async fn spawn_model_discovery_reports_the_live_models() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = r#"{"data":[{"id":"local-m","context_length":16384}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        });

        let mut providers = BTreeMap::new();
        let mut local = crate::config::providers::builtin_providers()
            .get("strata")
            .unwrap()
            .clone();
        local.name = "llama-cpp".into();
        local.base_url = format!("http://{addr}/v1");
        providers.insert("llama-cpp".into(), local);

        let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(1);
        spawn_model_discovery(providers, ui_tx);

        let event = ui_rx.recv().await.unwrap();
        let UiEvent::ModelsDiscovered(models) = event else {
            panic!("expected ModelsDiscovered, got {event:?}");
        };
        assert!(
            models
                .iter()
                .any(|m| m.provider == "llama-cpp" && m.model == "local-m"),
            "the discovered model id is in the rows: {models:?}"
        );
    }
}
