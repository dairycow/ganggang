//! Best-effort live model discovery via `GET /v1/models` on local
//! OpenAI-compatible servers (Strata, `llama-server`, Ollama, vLLM).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::Value;

use crate::config::ProviderConfig;

/// A model reported by a local server's `/v1/models` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    pub context_window: Option<u32>,
    pub status: Option<String>,
}

/// Derive `<base>/models` from an OpenAI-style `base_url`.
pub(crate) fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

/// True when `provider` speaks OpenAI Completions on `strata` or a loopback URL.
pub fn is_local_discoverable(provider: &ProviderConfig) -> bool {
    provider.api == super::ApiKind::OpenAiCompletions
        && (provider.name == "strata" || super::health::is_local_url(&provider.base_url))
}

/// Parse an OpenAI-compatible `/v1/models` JSON payload.
pub fn parse_models_response(body: &Value) -> Vec<DiscoveredModel> {
    let Some(items) = body.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let Some(id) = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !seen.insert(id.to_string()) {
            continue;
        }
        out.push(DiscoveredModel {
            id: id.to_string(),
            context_window: parse_context_window(item),
            status: parse_status(item),
        });
    }
    out
}

fn parse_status(item: &Value) -> Option<String> {
    let status = item.get("status")?;
    if let Some(s) = status.as_str() {
        let trimmed = s.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    status
        .get("value")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn positive_u32(val: Option<&Value>) -> Option<u32> {
    val.and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0)
}

fn parse_context_window(item: &Value) -> Option<u32> {
    positive_u32(item.get("max_model_len"))
        .or_else(|| positive_u32(item.get("context_length")))
        .or_else(|| positive_u32(item.get("max_context_length")))
        .or_else(|| positive_u32(item.get("meta").and_then(|m| m.get("n_ctx"))))
        .or_else(|| {
            item.get("status")
                .and_then(|s| s.get("args"))
                .and_then(|a| a.as_array())
                .and_then(|args| parse_ctx_from_args(args))
        })
        .or_else(|| {
            item.get("status")
                .and_then(|s| s.get("preset"))
                .and_then(|p| p.as_str())
                .and_then(parse_ctx_from_preset)
        })
}

fn parse_ctx_from_args(args: &[Value]) -> Option<u32> {
    let mut iter = args.iter().filter_map(|v| v.as_str());
    while let Some(arg) = iter.next() {
        if matches!(arg, "--ctx-size" | "-c" | "-ctx") {
            if let Some(val) = iter.next()
                && let Ok(n) = val.trim().parse::<u32>()
                && n > 0
            {
                return Some(n);
            }
            continue;
        }
        for prefix in ["--ctx-size=", "-c=", "-ctx="] {
            if let Some(rest) = arg.strip_prefix(prefix)
                && let Ok(n) = rest.trim().parse::<u32>()
                && n > 0
            {
                return Some(n);
            }
        }
    }
    None
}

fn parse_ctx_from_preset(preset: &str) -> Option<u32> {
    for line in preset.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if matches!(key.trim(), "ctx-size" | "c")
            && let Ok(n) = val.trim().parse::<u32>()
            && n > 0
        {
            return Some(n);
        }
    }
    None
}

fn build_probe_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
}

async fn probe_models_with_client(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Option<Vec<DiscoveredModel>> {
    let url = models_url(base_url);
    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.json::<Value>().await.ok()?;
    let models = parse_models_response(&body);
    (!models.is_empty()).then_some(models)
}

/// Probe `GET /v1/models` with short timeouts (500ms connect, 2s total).
/// Returns `None` on error, non-2xx status, invalid JSON, or empty list.
pub async fn probe_models(base_url: &str, api_key: Option<&str>) -> Option<Vec<DiscoveredModel>> {
    let client = build_probe_client()?;
    probe_models_with_client(&client, base_url, api_key).await
}

/// Concurrently probe all local OpenAI-compatible providers.
pub async fn discover_local_providers(
    providers: &BTreeMap<String, ProviderConfig>,
) -> BTreeMap<String, Vec<DiscoveredModel>> {
    let Some(client) = build_probe_client() else {
        return BTreeMap::new();
    };
    let mut join_set = tokio::task::JoinSet::new();
    for provider in providers.values().filter(|p| is_local_discoverable(p)) {
        let client = client.clone();
        let name = provider.name.clone();
        let base_url = provider.base_url.clone();
        let api_key = provider.api_key.clone();
        join_set.spawn(async move {
            let models = probe_models_with_client(&client, &base_url, api_key.as_deref()).await;
            (name, models)
        });
    }
    let mut discovered = BTreeMap::new();
    while let Some(res) = join_set.join_next().await {
        if let Ok((name, Some(models))) = res
            && !models.is_empty()
        {
            discovered.insert(name, models);
        }
    }
    discovered
}

#[cfg(test)]
mod tests;
