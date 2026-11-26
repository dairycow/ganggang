//! Error entries: the record a run leaves behind when it fails.

use super::*;

const ERROR: &str = "POST http://localhost/v1/chat/completions failed (503 Service Unavailable)";

fn error_entry(id: &str, parent_id: Option<&str>) -> Entry {
    Entry::Error {
        id: id.into(),
        parent_id: parent_id.map(String::from),
        timestamp: "2026-11-26T00:00:00Z".into(),
        error: ERROR.into(),
    }
}

/// The entry is `"type":"error"`, names its parent `parentId` like every
/// entry, and round-trips with and without one.
#[test]
fn an_error_entry_round_trips_as_its_own_type() {
    let line = serde_json::to_string(&error_entry("e2", Some("e1"))).unwrap();
    assert_eq!(
        line,
        r#"{"type":"error","id":"e2","parentId":"e1","timestamp":"2026-11-26T00:00:00Z","error":"POST http://localhost/v1/chat/completions failed (503 Service Unavailable)"}"#
    );
    let back: Entry = serde_json::from_str(&line).unwrap();
    assert!(
        matches!(&back, Entry::Error { error, .. } if error == ERROR),
        "{back:?}"
    );
    assert_eq!(last_entry_id(&back), "e2", "the next entry chains onto it");
    let orphan: Entry =
        serde_json::from_str(&serde_json::to_string(&error_entry("e1", None)).unwrap()).unwrap();
    assert!(matches!(
        orphan,
        Entry::Error {
            parent_id: None,
            ..
        }
    ));
}

/// An error entry is for the human reading the file: wherever it sits,
/// the wire view is the one without it, and no notice rides it.
#[test]
fn an_error_entry_never_reaches_the_wire() {
    let raw = include_str!("../../../../fixtures/session_minimal.jsonl");
    let base: Vec<Entry> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let expected = to_wire(&base[..3]);
    assert_eq!(expected.len(), 3);

    let mut history = base[..3].to_vec();
    history.insert(1, error_entry("e9", Some("a1b2c3d4")));
    assert_eq!(to_wire(&history), expected, "skipped in the middle");
    assert_eq!(
        to_wire_with(&history, ReplayReasoning::All),
        to_wire_with(&base[..3], ReplayReasoning::All),
    );

    history.push(error_entry("e10", Some("c3d4e5f6")));
    assert_eq!(to_wire(&history), expected, "skipped at the tail");
    assert_eq!(
        last_noticeable_entry_id(&history),
        last_noticeable_entry_id(&base[..3]),
        "it backs no message, so no notice can ride it"
    );
    // A resumed run chains onto it like any other entry.
    assert_eq!(last_entry_id(history.last().unwrap()), "e10");
}

/// What a failed run writes is what a resume reads back.
#[test]
fn a_persisted_error_entry_loads_with_its_history() {
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let mut store = io::SessionStore::create_under(root.path(), cwd.path(), "m").unwrap();
    store
        .append(&Entry::Message {
            id: "e1".into(),
            parent_id: None,
            timestamp: "t".into(),
            message: Message {
                role: Role::User,
                content: Some("hi".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: None,
            },
        })
        .unwrap();
    store.append(&error_entry("e2", Some("e1"))).unwrap();
    let path = store.path().to_path_buf();
    drop(store);

    let (_, entries, warnings) = io::load(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(entries.len(), 2);
    assert!(matches!(
        &entries[1],
        Entry::Error { parent_id: Some(parent), error, .. }
            if parent == "e1" && error == ERROR
    ));
    assert_eq!(to_wire(&entries).len(), 1, "the user message alone");
}
