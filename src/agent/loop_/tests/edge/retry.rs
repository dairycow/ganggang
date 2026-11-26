//! A retried request is the run's news. The client keeps retries to
//! itself and the loop passes each one on as a warning: in the TUI,
//! stderr is the screen, so a line printed there would land in the middle
//! of the transcript.

use crate::agent::loop_::tests::fake::{FakeServer, text_response};
use crate::agent::loop_::tests::{Registry, drive, event_values};

/// One whole raw HTTP response, as a server writes it.
fn raw(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn a_retried_request_is_reported_as_a_warning_in_the_event_stream() {
    let server = FakeServer::spawn_raw(vec![
        raw(
            "429 Too Many Requests",
            "Retry-After: 0\r\n",
            r#"{"error":"slow down"}"#,
        ),
        raw("200 OK", "", &text_response("fine")),
    ]);

    let (result, _, events) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(server.requests.lock().unwrap().len(), 2, "one retry");

    let values = event_values(&events);
    let warnings: Vec<&str> = values
        .iter()
        .filter(|e| e["type"] == "warned")
        .filter_map(|e| e["message"].as_str())
        .collect();
    assert_eq!(warnings.len(), 1, "{values:?}");
    assert!(
        warnings[0].contains("429 Too Many Requests") && warnings[0].contains("retry 1 of 5"),
        "{}",
        warnings[0]
    );
    // Said inside the turn it happened in: before that turn ends.
    let warned_at = values.iter().position(|e| e["type"] == "warned").unwrap();
    let ended_at = values
        .iter()
        .position(|e| e["type"] == "turn_ended")
        .unwrap();
    assert!(warned_at < ended_at, "{values:?}");
}
