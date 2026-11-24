//! The local `/health` probe: which URL and hosts it applies to, what it
//! fills in, and what it does with a server that answers wrong. Split from
//! the client so the client is the client.

use std::time::Duration;

use serde::Deserialize;

use super::ApiKind;

/// Permissive `/health` response from local servers (e.g. Strata).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HealthInfo {
    #[serde(default)]
    pub max_context: Option<u32>,
    #[serde(default)]
    pub kv_resident: Option<u32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
}

/// Derive `<base>/health` from an OpenAI-style `base_url` (stripping a
/// trailing `/` and `/v1`).
pub(crate) fn health_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    format!("{root}/health")
}

/// True when `base_url` points to a loopback host (`127.0.0.1`, `localhost`,
/// `[::1]`, etc.) without being fooled by URL userinfo.
pub(crate) fn is_local_url(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

enum HealthProbe {
    Ok(HealthInfo),
    HttpOther,
    Unreachable,
}

/// Probe `/health` on local/Strata endpoints to fill `context_window`,
/// `compact_threshold_tokens`, and (for Strata) the currently loaded model
/// when not explicitly configured, falling back to `/v1/models` if needed.
pub async fn maybe_probe_local_health(endpoint: &mut crate::config::Endpoint) {
    if endpoint.provider != "strata" && !is_local_url(&endpoint.base_url) {
        return;
    }
    if endpoint.context_window_explicit
        && endpoint.compact_threshold_explicit
        && endpoint.model_explicit
    {
        return;
    }
    let mut got_ctx = false;
    let mut got_model = false;
    match probe_health_detail(&endpoint.base_url).await {
        HealthProbe::Unreachable => return,
        HealthProbe::Ok(info) => {
            if !endpoint.context_window_explicit
                && let Some(max_ctx) = info.max_context.filter(|&c| c > 0)
            {
                endpoint.context_window = max_ctx;
                got_ctx = true;
            }
            if !endpoint.compact_threshold_explicit
                && let Some(kv) = info.kv_resident.filter(|&c| c > 0)
            {
                endpoint.compact_threshold_tokens = Some(kv);
            }
            if !endpoint.model_explicit
                && (endpoint.provider == "strata" || info.service.as_deref() == Some("strata"))
                && let Some(m) = info
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                endpoint.model = m.to_string();
                got_model = true;
            }
        }
        HealthProbe::HttpOther => {}
    }

    let need_strata_model = !endpoint.model_explicit && endpoint.provider == "strata" && !got_model;
    let need_ctx = !endpoint.context_window_explicit && !got_ctx;
    if (need_strata_model || need_ctx) && endpoint.api == ApiKind::OpenAiCompletions {
        let Some(models) =
            super::discovery::probe_models(&endpoint.base_url, endpoint.api_key.as_deref()).await
        else {
            return;
        };
        if need_strata_model {
            let picked = models
                .iter()
                .find(|m| m.status.as_deref() == Some("loaded"))
                .or_else(|| (models.len() == 1).then(|| &models[0]));
            if let Some(m) = picked {
                endpoint.model = m.id.clone();
            }
        }
        if need_ctx {
            let matched = models.iter().find(|m| m.id == endpoint.model).or_else(|| {
                (endpoint.provider == "strata" && models.len() == 1).then(|| &models[0])
            });
            if let Some(ctx) = matched.and_then(|m| m.context_window).filter(|&c| c > 0) {
                endpoint.context_window = ctx;
            }
        }
    }
}

async fn probe_health_detail(base_url: &str) -> HealthProbe {
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return HealthProbe::Unreachable;
    };
    let url = health_url(base_url);
    let Ok(resp) = client.get(&url).send().await else {
        return HealthProbe::Unreachable;
    };
    if !resp.status().is_success() {
        return HealthProbe::HttpOther;
    }
    match resp.json::<HealthInfo>().await {
        Ok(info) => HealthProbe::Ok(info),
        Err(_) => HealthProbe::HttpOther,
    }
}

/// Best-effort probe of `/health` with short timeouts (500ms connect, 2s
/// total). Returns `None` on any error, non-2xx status, or invalid JSON.
#[cfg(test)]
pub async fn probe_health(base_url: &str) -> Option<HealthInfo> {
    match probe_health_detail(base_url).await {
        HealthProbe::Ok(info) => Some(info),
        HealthProbe::HttpOther | HealthProbe::Unreachable => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn health_url_and_local_url_detection() {
        assert_eq!(
            health_url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/health"
        );
        assert_eq!(
            health_url("http://localhost:8080/"),
            "http://localhost:8080/health"
        );
        assert!(is_local_url("http://127.0.0.1:8080/v1"));
        assert!(is_local_url("http://127.0.0.2:8080/v1"));
        assert!(is_local_url("http://localhost:8080/v1"));
        assert!(is_local_url("http://[::1]:8080/v1"));
        assert!(is_local_url("http://[0:0:0:0:0:0:0:1]:8080/v1"));
        assert!(!is_local_url("https://api.deepseek.com"));
        assert!(!is_local_url("http://localhost:pass@evil.com/v1"));
        assert!(!is_local_url("http://127.0.0.1@evil.com/v1"));
    }

    #[tokio::test]
    async fn maybe_probe_local_health_updates_unexplicit_fields() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body = r#"{"max_context":262144,"kv_resident":98304}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            s.write_all(resp.as_bytes()).unwrap();
        });
        let mut ep = crate::config::Config::default().active_endpoint();
        ep.base_url = format!("http://{addr}/v1");
        ep.context_window_explicit = false;
        ep.compact_threshold_explicit = false;
        maybe_probe_local_health(&mut ep).await;
        assert_eq!(ep.context_window, 262_144);
        assert_eq!(ep.compact_threshold_tokens, Some(98_304));
    }

    #[tokio::test]
    async fn probe_health_parses_strata_and_handles_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (status, body) in [
                (
                    "200 OK",
                    r#"{"status":"ok","max_context":262144,"kv_resident":65536,"model":"qwen3.8","service":"strata"}"#,
                ),
                ("404 Not Found", "nope"),
                ("200 OK", "{not json"),
            ] {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).unwrap();
            }
        });

        let base = format!("http://{addr}/v1");
        let info = probe_health(&base).await.expect("valid health");
        assert_eq!(info.max_context, Some(262_144));
        assert_eq!(info.kv_resident, Some(65_536));
        assert_eq!(info.service.as_deref(), Some("strata"));

        assert!(probe_health(&base).await.is_none());
        assert!(probe_health(&base).await.is_none());
    }

    #[tokio::test]
    async fn maybe_probe_local_health_adopts_strata_model_unless_explicit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body =
                    r#"{"max_context":32768,"model":"inkling-2-mini-sq-15","service":"strata"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                s.write_all(resp.as_bytes()).unwrap();
            }
        });

        let mut ep = crate::config::Config::default().active_endpoint();
        ep.base_url = format!("http://{addr}/v1");
        ep.model_explicit = false;
        ep.context_window_explicit = false;
        maybe_probe_local_health(&mut ep).await;
        assert_eq!(ep.model, "inkling-2-mini-sq-15");
        assert_eq!(ep.context_window, 32_768);

        let mut explicit_ep = crate::config::Config::default().active_endpoint();
        explicit_ep.base_url = format!("http://{addr}/v1");
        explicit_ep.model = "my-forced-model".into();
        explicit_ep.model_explicit = true;
        maybe_probe_local_health(&mut explicit_ep).await;
        assert_eq!(explicit_ep.model, "my-forced-model");
    }

    #[tokio::test]
    async fn maybe_probe_local_health_falls_back_to_v1_models_for_ctx_and_strata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if req.starts_with("GET /health ") {
                    r#"{"status":"ok"}"#
                } else {
                    r#"{"data":[{"id":"gemma4-26b-200k","status":{"value":"loaded","args":["llama-server","--ctx-size","200000"]}}]}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                s.write_all(resp.as_bytes()).unwrap();
            }
        });

        let mut custom_ep = crate::config::Config::default().active_endpoint();
        custom_ep.provider = "llama-cpp".into();
        custom_ep.model = "gemma4-26b-200k".into();
        custom_ep.model_explicit = true;
        custom_ep.context_window_explicit = false;
        custom_ep.base_url = format!("http://{addr}/v1");
        maybe_probe_local_health(&mut custom_ep).await;
        assert_eq!(custom_ep.model, "gemma4-26b-200k");
        assert_eq!(custom_ep.context_window, 200_000);

        let mut strata_ep = crate::config::Config::default().active_endpoint();
        strata_ep.base_url = format!("http://{addr}/v1");
        strata_ep.model_explicit = false;
        strata_ep.context_window_explicit = false;
        maybe_probe_local_health(&mut strata_ep).await;
        assert_eq!(strata_ep.model, "gemma4-26b-200k");
        assert_eq!(strata_ep.context_window, 200_000);
    }
}
