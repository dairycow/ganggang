//! Export tests: escaping is the security boundary, self-containment is
//! the delivery contract, splicing keeps calls with their results.

use std::path::Path;

use serde_json::json;

use super::{default_out_path, esc, export_file, render_html, resolve_target};
use crate::agent::session::io::{self, SessionHeader};
use crate::agent::session::{Entry, Message, Role, ToolCall};

fn header() -> SessionHeader {
    serde_json::from_str(
        r#"{"type":"session","version":1,"id":"abc123","timestamp":"2026-11-23T00:00:00Z",
            "cwd":"/tmp/x","model":"m"}"#,
    )
    .unwrap()
}

fn msg(id: &str, parent: Option<&str>, message: Message) -> Entry {
    Entry::Message {
        id: id.into(),
        parent_id: parent.map(String::from),
        timestamp: "2026-11-23T00:00:00Z".into(),
        message,
    }
}

/// Every field explicit — the base the helpers below fill in over.
fn base_message() -> Message {
    Message {
        role: Role::User,
        content: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: None,
        is_error: false,
        reasoning: None,
    }
}

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(text.into()),
        ..base_message()
    }
}

fn assistant(text: Option<&str>, reasoning: Option<&str>, calls: Vec<ToolCall>) -> Message {
    Message {
        role: Role::Assistant,
        content: text.map(String::from),
        reasoning: reasoning.map(String::from),
        tool_calls: calls,
        ..base_message()
    }
}

fn tool_result(id: &str, text: &str, is_error: bool) -> Message {
    Message {
        role: Role::ToolResult,
        content: Some(text.into()),
        tool_call_id: Some(id.into()),
        is_error,
        ..base_message()
    }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args,
        args_error: None,
    }
}

/// The hostile payload every content kind gets fed: if any of it
/// survives raw, the escaper has a hole.
const HOSTILE: &str = "</script><img src=x onerror=\"alert(1)\">&<>\"'";

#[test]
fn escapes_html_in_every_content_kind() {
    let entries = vec![
        msg("e1", None, user(HOSTILE)),
        msg(
            "e2",
            Some("e1"),
            assistant(
                Some(HOSTILE),
                Some(HOSTILE),
                vec![call("c1", HOSTILE, json!({ "k": HOSTILE }))],
            ),
        ),
        msg("e3", Some("e2"), tool_result("c1", HOSTILE, false)),
        Entry::Compaction {
            id: "e4".into(),
            parent_id: Some("e3".into()),
            timestamp: "t".into(),
            summary: HOSTILE.into(),
            first_kept_id: "e2".into(),
        },
    ];
    let html = render_html(&header(), &entries);

    assert!(!html.contains(HOSTILE), "raw hostile text must not survive");
    assert!(!html.contains("<script"), "no script tags at all");
    assert!(!html.contains("<img"), "no injected elements");
    assert!(html.contains("&lt;/script&gt;"), "content escaped");
    assert!(html.contains("&amp;"), "ampersand escaped");
}

#[test]
fn splices_results_into_their_calls_with_status() {
    let entries = vec![
        msg("e1", None, user("look")),
        msg(
            "e2",
            Some("e1"),
            assistant(
                None,
                None,
                vec![
                    call("c1", "read", json!({"path": "x"})),
                    call("c2", "bash", json!({})),
                ],
            ),
        ),
        msg("e3", Some("e2"), tool_result("c1", "file body", false)),
        msg("e4", Some("e3"), tool_result("c2", "boom", true)),
    ];
    let html = render_html(&header(), &entries);

    let call_block = html
        .split("<summary>▸ read")
        .nth(1)
        .expect("read call rendered");
    assert!(
        html.contains("<summary>▸ read x</summary>"),
        "the summary shows the path, not its JSON: {html}"
    );
    assert!(
        call_block.contains("→ file body</pre></details>"),
        "result rides inside its call: {call_block}"
    );
    assert!(
        call_block.contains("→ ✗ boom") || html.contains("→ ✗ boom"),
        "error result carries the mark"
    );
    // Exactly one standalone tool block: none (both results spliced).
    assert!(
        !html.contains("<div class=\"tool\"><pre>→"),
        "no standalone duplicates"
    );
}

#[test]
fn dangling_result_renders_standalone() {
    let entries = vec![
        msg("e1", None, user("hi")),
        msg(
            "e2",
            Some("e1"),
            tool_result("ghost", "orphan output", false),
        ),
    ];
    let html = render_html(&header(), &entries);
    assert!(
        html.contains("<div class=\"tool\"><pre>→ orphan output"),
        "{html}"
    );
}

#[test]
fn compaction_and_thinking_render() {
    let entries = vec![
        msg("e1", None, user("x")),
        msg(
            "e2",
            Some("e1"),
            assistant(Some("answer"), Some("hmm"), vec![]),
        ),
        Entry::Compaction {
            id: "e3".into(),
            parent_id: Some("e2".into()),
            timestamp: "t".into(),
            summary: "the story so far".into(),
            first_kept_id: "e2".into(),
        },
    ];
    let html = render_html(&header(), &entries);
    assert!(
        html.contains("details class=\"thinking\""),
        "thinking collapses"
    );
    assert!(html.contains("context compacted"), "compaction marked");
    assert!(html.contains("the story so far"), "summary present");
}

/// A run's recorded error renders as its own marked block, escaped like
/// every other kind of text.
#[test]
fn an_error_entry_renders_marked_and_escaped() {
    let entries = vec![
        msg("e1", None, user("hi")),
        Entry::Error {
            id: "e2".into(),
            parent_id: Some("e1".into()),
            timestamp: "2026-11-26T00:00:00Z".into(),
            error: HOSTILE.into(),
        },
    ];
    let html = render_html(&header(), &entries);
    assert!(
        html.contains("<div class=\"error\" title=\"2026-11-26T00:00:00Z\"><pre>✗ "),
        "{html}"
    );
    assert!(
        html.contains("<pre>✗ &lt;/script&gt;"),
        "the error text is escaped: {html}"
    );
    assert!(!html.contains(HOSTILE), "raw hostile text must not survive");
    assert!(html.contains(".error {"), "the block is styled");
}

#[test]
fn the_document_is_self_contained() {
    let html = render_html(&header(), &[msg("e1", None, user("hi"))]);
    assert!(!html.contains("src=\""), "no external sources");
    assert!(!html.contains("href=\""), "no external links");
    assert!(!html.contains("<script"), "no scripts");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<style>"), "inline CSS present");
}

#[test]
fn esc_covers_the_five_characters() {
    assert_eq!(esc("&<>\"'"), "&amp;&lt;&gt;&quot;&#39;");
    assert_eq!(esc("plain"), "plain");
}

#[test]
fn resolves_a_direct_path_and_names_a_default_output() {
    let tmp = std::env::temp_dir().join(format!("gg-export-t-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let session = tmp.join("20261123-000000-deadbeef.jsonl");
    std::fs::write(
        &session,
        "{\"type\":\"session\",\"version\":1,\"id\":\"u\",\"timestamp\":\"t\",\"cwd\":\"/\",\"model\":\"m\"}\n",
    )
    .unwrap();

    assert_eq!(
        resolve_target(session.to_str().unwrap(), &tmp).unwrap(),
        session
    );
    let out = default_out_path(&session, &tmp);
    assert_eq!(out, tmp.join("20261123-000000-deadbeef.html"));

    // And export_file round-trips a real store's file to HTML on disk.
    let mut store = io::SessionStore::create_under(&tmp, &tmp, "m").unwrap();
    store
        .append(&msg("e1", None, user("round trip </b>")))
        .unwrap();
    let written = store.path().to_path_buf();
    drop(store);
    let out = tmp.join("out.html");
    export_file(&written, &out).unwrap();
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.contains("round trip &lt;/b&gt;"), "{html}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn an_unknown_spec_without_a_file_is_an_error() {
    let tmp = std::env::temp_dir().join(format!("gg-export-miss-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    // A bare spec resolves through this directory's session store, which
    // does not exist for a fresh temp dir — and a missing file path is
    // not a session either. Either way: an error, never a guess.
    assert!(resolve_target("deadbeef", &tmp).is_err());
    assert!(resolve_target(tmp.join("nope.jsonl").to_str().unwrap(), &tmp).is_err());
    let _ = std::fs::remove_dir_all(&tmp);
}

// Path::is_file on a missing path is false — pin the helper honestly.
#[test]
fn a_path_spec_must_exist() {
    assert!(!Path::new("/definitely/not/here.jsonl").is_file());
}
