//! A run that fails leaves its error in the session: the file says why the
//! history stops where it does, and the error never reaches the model.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::super::fake::FakeServer;
use super::super::{Registry, drive, drive_with_store, event_values};
use crate::agent::session::io::{SessionStore, load};
use crate::agent::session::{self, Entry, Role};

/// A whole raw HTTP response, as a server writes it.
fn raw(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The server's answer to a request it will not serve. A 400 is not
/// retried, so one response ends the run at once.
fn refused() -> String {
    raw("400 Bad Request", r#"{"error":"bad model name"}"#)
}

/// The error is recorded in memory and on disk, chained onto the history,
/// carrying the text the caller gets; the history alone is what the next
/// request would carry.
#[tokio::test]
async fn a_failed_run_records_its_error_in_the_session() {
    let server = FakeServer::spawn_raw(vec![refused()]);
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let store = SessionStore::create_under(root.path(), cwd.path(), "fake-model").unwrap();
    let path = store.path().to_path_buf();

    let (result, entries, events) = drive_with_store(
        &server.base_url,
        Registry::new(),
        "hi",
        Arc::new(AtomicBool::new(false)),
        Some(store),
    )
    .await;
    let err = format!("{:#}", result.unwrap_err());
    assert_eq!(
        server.requests.lock().unwrap().len(),
        1,
        "a 400 is not retried"
    );

    assert_eq!(
        event_values(&events),
        vec![
            json!({"type": "run_started"}),
            json!({"type": "turn_started"})
        ],
        "no turn ended and no run ended: the request failed"
    );
    assert_eq!(entries.len(), 2, "the user message and the error");
    let Entry::Message {
        id: user_id,
        message,
        ..
    } = &entries[0]
    else {
        panic!("the run's input leads")
    };
    assert_eq!(message.role, Role::User);
    let Entry::Error {
        error, parent_id, ..
    } = entries.last().unwrap()
    else {
        panic!("the run's error is the last entry")
    };
    assert!(
        error.contains("400 Bad Request") && error.contains("bad model name"),
        "{error}"
    );
    assert_eq!(error, &err, "the entry carries the error the caller gets");
    assert_eq!(
        parent_id.as_deref(),
        Some(user_id.as_str()),
        "chained onto the history"
    );
    assert_eq!(
        session::to_wire(&entries).len(),
        1,
        "the error is never sent to the model"
    );

    // Disk and memory agree.
    let (_, on_disk, warnings) = load(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        serde_json::to_value(&on_disk).unwrap(),
        serde_json::to_value(&entries).unwrap()
    );
}

/// Without a session store the error is still kept in memory.
#[tokio::test]
async fn a_failed_run_keeps_its_error_without_a_session() {
    let server = FakeServer::spawn_raw(vec![refused()]);
    let (result, entries, _) = drive(&server.base_url, Registry::new(), "hi").await;
    assert!(result.is_err());
    assert!(
        matches!(entries.last(), Some(Entry::Error { .. })),
        "{entries:?}"
    );
}
