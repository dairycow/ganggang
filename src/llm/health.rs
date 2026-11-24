//! The local `/health` probe: which URL and hosts it applies to, what it
//! fills in, and what it does with a server that answers wrong. Split from
//! the client so the client is the client.

use std::time::Duration;

use serde::Deserialize;

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

/// Probe `/health` on local/Strata endpoints to fill `context_window` and
/// `compact_threshold_tokens` when not explicitly configured.
pub async fn maybe_probe_local_health(endpoint: &mut crate::config::Endpoint) {
    if endpoint.context_window_explicit && endpoint.compact_threshold_explicit {
        return;
    }
    if endpoint.provider != "strata" && !is_local_url(&endpoint.base_url) {
        return;
    }
    let Some(info) = probe_health(&endpoint.base_url).await else {
        return;
    };
    if !endpoint.context_window_explicit
        && let Some(max_ctx) = info.max_context.filter(|&c| c > 0)
    {
        endpoint.context_window = max_ctx;
    }
    if !endpoint.compact_threshold_explicit
        && let Some(kv) = info.kv_resident.filter(|&c| c > 0)
    {
        endpoint.compact_threshold_tokens = Some(kv);
    }
}

/// Best-effort probe of `/health` with short timeouts (500ms connect, 2s
/// total). Returns `None` on any error, non-2xx status, or invalid JSON.
pub async fn probe_health(base_url: &str) -> Option<HealthInfo> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let url = health_url(base_url);
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<HealthInfo>().await.ok()
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
}
