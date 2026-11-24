use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[test]
fn parses_strata_llama_router_and_plain_models() {
    let body = json!({
        "object": "list",
        "data": [
            {
                "id": "inkling-2-mini-sq-15",
                "status": {"value": "loaded"},
                "max_model_len": 32768,
                "meta": {"n_ctx": 32768, "n_ctx_train": 131072}
            },
            {
                "id": "gemma4-26b-200k",
                "status": {
                    "value": "unloaded",
                    "args": ["llama-server", "--ctx-size", "200000"]
                }
            },
            {
                "id": "qwen-from-preset",
                "status": {
                    "value": "sleeping",
                    "preset": "model = x.gguf\nctx-size = 65536\n"
                }
            },
            {
                "id": "inline-flag",
                "status": "loaded",
                "meta": {"n_ctx_train": 262144}
            },
            {
                "id": "inline-flag-c",
                "status": {
                    "value": "unloaded",
                    "args": ["llama-server", "-c=16384"]
                }
            },
            {
                "id": "   "
            },
            {
                "id": "inkling-2-mini-sq-15",
                "max_model_len": 999
            }
        ]
    });

    let parsed = parse_models_response(&body);
    assert_eq!(parsed.len(), 5);
    assert_eq!(
        parsed[0],
        DiscoveredModel {
            id: "inkling-2-mini-sq-15".into(),
            context_window: Some(32768),
            status: Some("loaded".into()),
        }
    );
    assert_eq!(
        parsed[1],
        DiscoveredModel {
            id: "gemma4-26b-200k".into(),
            context_window: Some(200_000),
            status: Some("unloaded".into()),
        }
    );
    assert_eq!(
        parsed[2],
        DiscoveredModel {
            id: "qwen-from-preset".into(),
            context_window: Some(65_536),
            status: Some("sleeping".into()),
        }
    );
    assert_eq!(
        parsed[3],
        DiscoveredModel {
            id: "inline-flag".into(),
            context_window: None,
            status: Some("loaded".into()),
        }
    );
    assert_eq!(
        parsed[4],
        DiscoveredModel {
            id: "inline-flag-c".into(),
            context_window: Some(16_384),
            status: Some("unloaded".into()),
        }
    );
}

#[tokio::test]
async fn probe_models_handles_auth_and_failures() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen_auth = Arc::new(Mutex::new(String::new()));
    let seen_auth_clone = seen_auth.clone();

    std::thread::spawn(move || {
        for (status, body) in [
            ("200 OK", r#"{"data":[{"id":"m1","max_model_len":8192}]}"#),
            ("404 Not Found", "nope"),
            ("200 OK", "{bad json"),
        ] {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            if seen_auth_clone.lock().unwrap().is_empty() {
                *seen_auth_clone.lock().unwrap() = req;
            }
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        }
    });

    let base = format!("http://{addr}/v1");
    let models = probe_models(&base, Some("secret-key")).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "m1");
    assert!(
        seen_auth
            .lock()
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-key")
    );
    assert!(probe_models(&base, None).await.is_none());
    assert!(probe_models(&base, None).await.is_none());
}

#[tokio::test]
async fn discover_local_providers_probes_only_local_openai() {
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

    let mut cloud = crate::config::providers::builtin_providers()
        .get("deepseek")
        .unwrap()
        .clone();
    cloud.base_url = "https://127.0.0.1:1/v1".into(); // non-local? no, it's local URL, so use example.com
    cloud.base_url = "https://api.deepseek.com".into();
    providers.insert("deepseek".into(), cloud);

    let mut anthropic_local = crate::config::providers::builtin_providers()
        .get("strata")
        .unwrap()
        .clone();
    anthropic_local.name = "local-anth".into();
    anthropic_local.api = crate::llm::ApiKind::AnthropicMessages;
    providers.insert("local-anth".into(), anthropic_local);

    let found = discover_local_providers(&providers).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found["llama-cpp"][0].id, "local-m");
}
