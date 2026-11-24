//! Tests for the local `/health` probe: the URL it derives, which hosts
//! count as local, what it fills in, and what it does with a server that
//! answers wrong.

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
