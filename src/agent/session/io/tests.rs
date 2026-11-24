//! Tests for [`super`]: golden round-trip, corrupt tolerance, listing,
//! resume discovery.

use std::path::{Path, PathBuf};

use super::super::{Entry, Message, Role, ToolCall};
use super::*;

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gg-session-io-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn golden_file_loads_and_reserializes_line_by_line() {
    let raw = include_str!("../../../../fixtures/session_golden.jsonl");
    let path = write_to_temp(tag_of(raw), raw);

    let (header, entries, warnings) = load(&path).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(header.kind, "session");
    assert_eq!(header.version, 1);
    assert_eq!(header.model, "qwen3.8-flash-next-iq3_s");

    // Entry-by-entry byte equality with the fixture: the file format is
    // pinned, not just the parsed shape.
    let lines: Vec<&str> = raw.lines().skip(1).collect();
    assert_eq!(entries.len(), lines.len());
    for (entry, line) in entries.iter().zip(&lines) {
        assert_eq!(&serde_json::to_string(entry).unwrap(), line);
    }

    // Replay under the final compaction (§14): the golden boundary names
    // the last tool_result, so the view snaps forward to the next user
    // message — there is none — and the summary note carries the context
    // alone. An orphaned tool message is never sent.
    let wire = super::super::to_wire(&entries);
    assert_eq!(wire.len(), 1);
    assert_eq!(wire[0].role, "user");
    let content = wire[0].content.as_deref().unwrap();
    assert!(content.contains("[conversation before this point was compacted]"));
    assert!(content.contains("off-by-one"));
}

fn tag_of(raw: &str) -> String {
    let hash: u64 = raw.bytes().map(|b| b as u64).sum();
    hash.to_string()
}

fn write_to_temp(tag: String, content: &str) -> PathBuf {
    let path = tmp_root(&tag).join("golden.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn corrupt_lines_anywhere_are_skipped_with_warnings() {
    let raw = include_str!("../../../../fixtures/session_golden.jsonl");
    let torn: String =
        raw.lines()
            .take(3)
            .chain(["{\"type\":\"mess"])
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
    let path = write_to_temp(tag_of(&torn), &torn);

    let (_, entries, warnings) = load(&path).unwrap();
    assert_eq!(entries.len(), 2, "valid prefix survives");
    assert_eq!(warnings.len(), 1, "torn tail is reported: {warnings:?}");
}

#[test]
fn a_missing_or_wrong_header_is_fatal() {
    let no_header = "{\"type\":\"message\",\"id\":\"a\",\"parentId\":null,\"timestamp\":\"t\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
    let path = write_to_temp(tag_of(no_header), no_header);
    assert!(load(&path).is_err());

    let empty = "";
    let path = write_to_temp(tag_of(empty), empty);
    assert!(load(&path).is_err());
}

#[test]
fn future_versions_are_rejected_not_misparsed() {
    let raw = include_str!("../../../../fixtures/session_golden.jsonl").replacen(
        "\"version\":1",
        "\"version\":2",
        1,
    );
    let path = write_to_temp(tag_of(&raw), &raw);
    let err = load(&path).unwrap_err();
    assert!(format!("{err:#}").contains("version 2"), "{err:#}");
}

#[test]
fn appended_conversation_round_trips_through_the_file() {
    let root = tmp_root("roundtrip");
    let cwd = Path::new("/proj/x");
    let mut store = SessionStore::create_under(&root, cwd, "m").unwrap();

    // A scripted conversation covering every entry shape, including an
    // args_error call and a compaction record.
    let original = vec![
        entry_message(Some("hello"), vec![], None, false),
        Entry::Message {
            id: "b2".into(),
            parent_id: Some("b1".into()),
            timestamp: "t".into(),
            message: Message {
                role: Role::Assistant,
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    args: serde_json::json!({}),
                    args_error: Some("bad json".into()),
                }],
                tool_call_id: None,
                usage: None,
                is_error: false,
                reasoning: None,
            },
        },
        entry_message(Some("out"), vec![], Some("call_1".into()), true),
        Entry::Compaction {
            id: "b4".into(),
            parent_id: Some("b3".into()),
            timestamp: "t".into(),
            summary: "s".into(),
            first_kept_id: "b3".into(),
        },
    ];
    for entry in &original {
        store.append(entry).unwrap();
    }

    let path = store.path().to_path_buf();
    let (_, loaded, warnings) = load(&path).unwrap();
    assert!(warnings.is_empty());

    // ChatMessage carries no PartialEq; the wire form is comparable via its
    // JSON, which is also the stronger claim (byte-identical requests).
    assert_eq!(
        serde_json::to_string(&super::super::to_wire(&loaded)).unwrap(),
        serde_json::to_string(&super::super::to_wire(&original)).unwrap(),
        "replay of the file equals replay of memory"
    );
    assert_eq!(loaded.len(), original.len());
}

fn entry_message(
    content: Option<&str>,
    _calls: Vec<()>,
    tool_call_id: Option<String>,
    is_error: bool,
) -> Entry {
    Entry::Message {
        id: "b1".into(),
        parent_id: None,
        timestamp: "t".into(),
        message: Message {
            role: if tool_call_id.is_some() {
                Role::ToolResult
            } else {
                Role::User
            },
            content: content.map(String::from),
            tool_calls: Vec::new(),
            tool_call_id,
            usage: None,
            is_error,
            reasoning: None,
        },
    }
}

#[test]
fn the_header_is_written_once_no_matter_how_many_appends() {
    let root = tmp_root("header-once");
    let mut store = SessionStore::create_under(&root, Path::new("/proj/y"), "m").unwrap();
    for _ in 0..5 {
        store
            .append(&entry_message(Some("x"), vec![], None, false))
            .unwrap();
    }
    let text = std::fs::read_to_string(store.path()).unwrap();
    let headers = text.lines().filter(|l| l.contains("\"type\":\"session\""));
    assert_eq!(headers.count(), 1);
    assert_eq!(text.lines().count(), 6);
}

#[test]
fn session_paths_slug_the_cwd_and_stamp_the_time() {
    let root = tmp_root("paths");
    let store =
        SessionStore::create_under(&root, Path::new("/home/user/Projects/ganggang"), "m").unwrap();
    let path = store.path();
    assert_eq!(
        path.parent().unwrap().file_name().unwrap(),
        "-home-user-Projects-ganggang"
    );
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.ends_with(".jsonl"), "{name}");
    // YYYYMMDD-HHMMSS-8hex.jsonl
    let stem = name.trim_end_matches(".jsonl");
    let parts: Vec<&str> = stem.splitn(3, '-').collect();
    assert_eq!(parts.len(), 3, "{name}");
    assert_eq!(parts[0].len(), 8, "date: {name}");
    assert_eq!(parts[1].len(), 6, "time with seconds: {name}");
    assert_eq!(parts[2].len(), 8, "id: {name}");
    assert!(parts[2].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn listing_is_newest_first_and_prefix_matching_is_strict() {
    let root = tmp_root("list");
    let cwd = Path::new("/proj/z");
    let dir = root.join(slug(cwd));
    std::fs::create_dir_all(&dir).unwrap();
    // Same minute: newest-ness must come from the id tiebreak too — write
    // three files with known ids by hand-rolling headers.
    for (stamp, id) in [
        ("20260101-0900", "aaaa1111-0000"),
        ("20260101-0901", "bbbb2222-0000"),
        ("20260102-0800", "aacc3333-0000"),
    ] {
        let path = dir.join(format!("{stamp}-{id}.jsonl"));
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session\",\"version\":1,\"id\":\"{id}\",\"timestamp\":\"t\",\"cwd\":\"/proj/z\",\"model\":\"m\"}}\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(dir.join("not-a-session.txt"), "x").unwrap();

    let (sessions, warnings) = list_sessions(&dir);
    assert!(warnings.is_empty());
    assert_eq!(
        sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["aacc3333-0000", "bbbb2222-0000", "aaaa1111-0000"],
        "filename sort, newest first"
    );

    // Prefix resolution against the same listing: ambiguous lists ids,
    // unique resolves, missing errors.
    let ambiguous = resolve_prefix(&sessions, "aa").unwrap_err();
    assert!(
        format!("{ambiguous:#}").contains("ambiguous"),
        "{ambiguous:#}"
    );
    assert!(format!("{ambiguous:#}").contains("aaaa1111-0000"));

    let one = resolve_prefix(&sessions, "bb").unwrap();
    assert_eq!(one.id, "bbbb2222-0000");

    // The 8-hex filename id is also an acceptable prefix — it is the
    // handle users see in the session: line.
    let by_file = resolve_prefix(&sessions, "aaaa1111").unwrap();
    assert_eq!(by_file.id, "aaaa1111-0000");

    assert!(resolve_prefix(&sessions, "zz").is_err());

    // And -c picks the newest.
    assert_eq!(latest_in(&dir).unwrap().id, "aacc3333-0000");
}

#[test]
fn header_only_sessions_sort_below_conversations_but_stay_resolvable() {
    let root = tmp_root("header-only");
    let dir = root.join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let header = |id: &str| {
        format!(
            "{{\"type\":\"session\",\"version\":1,\"id\":\"{id}\",\"timestamp\":\"t\",\"cwd\":\"/proj/z\",\"model\":\"m\"}}\n"
        )
    };
    // A newer, empty (watch-materialized) file and an older real conversation.
    std::fs::write(
        dir.join("20260102-1000-eeee5555.jsonl"),
        header("eeee5555-0000"),
    )
    .unwrap();
    let entry = serde_json::to_string(&entry_message(Some("hi"), vec![], None, false)).unwrap();
    std::fs::write(
        dir.join("20260101-0900-ffff6666.jsonl"),
        format!("{}{entry}\n", header("ffff6666-0000")),
    )
    .unwrap();

    let (sessions, warnings) = list_sessions(&dir);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["ffff6666-0000", "eeee5555-0000"],
        "a real conversation outranks a newer empty one"
    );
    assert_eq!(latest_in(&dir).unwrap().id, "ffff6666-0000");
    // The empty session is still addressable by id, which is what a watch
    // restart uses before the first message.
    assert_eq!(
        resolve_prefix(&sessions, "eeee").unwrap().id,
        "eeee5555-0000"
    );
}

#[test]
fn open_existing_refuses_to_extend_a_corrupt_file() {
    let root = tmp_root("reopen");
    let mut store = SessionStore::create_under(&root, Path::new("/proj/w"), "m").unwrap();
    store
        .append(&entry_message(Some("x"), vec![], None, false))
        .unwrap();
    // Simulate a torn trailing write.
    let intact = std::fs::read_to_string(store.path()).unwrap();
    std::fs::write(store.path(), format!("{intact}{{\"type\":\"mess")).unwrap();
    let err = match SessionStore::open_existing(store.path()) {
        Err(e) => e,
        Ok(_) => panic!("a corrupt session must not be extended"),
    };
    assert!(format!("{err:#}").contains("corrupt"), "{err:#}");
}

#[test]
fn a_new_store_materializes_only_on_the_first_append() {
    let root = tmp_root("lazy");
    let mut store = SessionStore::create_under(&root, Path::new("/proj/lazy"), "m").unwrap();
    assert!(
        !store.path().exists(),
        "launch-and-quit must not leave a header-only file"
    );
    store
        .append(&entry_message(Some("x"), vec![], None, false))
        .unwrap();
    assert!(store.path().exists());
    let text = std::fs::read_to_string(store.path()).unwrap();
    let mut lines = text.lines();
    assert!(lines.next().unwrap().contains("\"type\":\"session\""));
    assert_eq!(text.lines().count(), 2, "header + one entry");
}

#[test]
fn a_torn_trailing_newline_is_repaired_so_appends_never_concatenate() {
    let root = tmp_root("torn");
    let cwd = Path::new("/proj/torn");
    let mut store = SessionStore::create_under(&root, cwd, "m").unwrap();
    store
        .append(&entry_message(Some("one"), vec![], None, false))
        .unwrap();
    store
        .append(&entry_message(Some("two"), vec![], None, false))
        .unwrap();
    let path = store.path().to_path_buf();
    drop(store);

    // Simulate a crash between the entry text and its newline.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with('\n'));
    std::fs::write(&path, text.trim_end_matches('\n')).unwrap();

    let mut resumed = SessionStore::open_existing(&path).unwrap();
    resumed
        .append(&entry_message(Some("three"), vec![], None, false))
        .unwrap();
    let (_, entries, warnings) = load(&path).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(entries.len(), 3, "no entry lost to concatenation");
}

#[test]
fn resume_repairs_dangling_tool_calls_on_both_disk_and_memory() {
    let root = tmp_root("dangling");
    let mut store = SessionStore::create_under(&root, Path::new("/proj/d"), "m").unwrap();
    // user, assistant(two calls), result for call_1 only — the shape a
    // kill -9 mid-batch leaves behind.
    store
        .append(&entry_message(Some("go"), vec![], None, false))
        .unwrap();
    let assistant = Entry::Message {
        id: "a1".into(),
        parent_id: None,
        timestamp: "t".into(),
        message: Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    args: serde_json::json!({}),
                    args_error: None,
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "bash".into(),
                    args: serde_json::json!({}),
                    args_error: None,
                },
            ],
            tool_call_id: None,
            usage: None,
            is_error: false,
            reasoning: None,
        },
    };
    store.append(&assistant).unwrap();
    store
        .append(&entry_message(
            Some("ok"),
            vec![],
            Some("call_1".into()),
            false,
        ))
        .unwrap();
    let path = store.path().to_path_buf();
    drop(store);

    let mut store = SessionStore::open_existing(&path).unwrap();
    let (_, mut entries, _) = load(&path).unwrap();
    assert_eq!(entries.len(), 3);
    let repaired = repair_dangling_tool_calls(&mut entries, &mut store).unwrap();
    assert_eq!(repaired, 1, "call_2 never got its result");

    // Disk and memory agree, and replay never carries a dangling call.
    let (_, disk_entries, _) = load(&path).unwrap();
    assert_eq!(disk_entries.len(), 4);
    let answered: Vec<String> = entries
        .iter()
        .chain(disk_entries.iter())
        .filter_map(|e| match e {
            Entry::Message { message, .. } => message.tool_call_id.clone(),
            _ => None,
        })
        .collect();
    assert!(answered.contains(&"call_1".to_string()));
    assert!(answered.contains(&"call_2".to_string()));
    for message in super::super::to_wire(&entries)
        .iter()
        .filter(|m| m.role == "assistant")
    {
        for call in &message.tool_calls {
            assert!(
                answered.contains(&call.id),
                "replay carries a result for {}",
                call.id
            );
        }
    }
}

#[test]
fn a_created_session_is_discoverable_in_its_cwd_dir() {
    // The composition main uses: create under a root, list in
    // root/<slug>. A past bug double-slugged the path (root already
    // contained the slug), making every created session undiscoverable.
    let root = tmp_root("discover");
    let cwd = Path::new("/proj/discover");
    let mut store = SessionStore::create_under(&root, cwd, "m").unwrap();
    store
        .append(&entry_message(Some("x"), vec![], None, false))
        .unwrap();

    let dir = root.join(slug(cwd));
    let found = latest_in(&dir).expect("created session is listed");
    assert_eq!(found.path, store.path());
}

#[test]
fn create_at_handles_missing_and_empty_files_and_refuses_non_empty() {
    let root = tmp_root("create-at");
    let nested = root.join("sub").join("custom.jsonl");
    let mut store = SessionStore::create_at(nested.clone(), Path::new("/proj/c"), "m").unwrap();
    store
        .append(&entry_message(Some("hello"), vec![], None, false))
        .unwrap();
    let (_, entries, warnings) = load(&nested).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(entries.len(), 1);

    // Non-empty existing file is refused.
    assert!(SessionStore::create_at(nested, Path::new("/proj/c"), "m").is_err());

    // 0-byte existing file (like mktemp) is initialized cleanly.
    let empty = root.join("empty.jsonl");
    std::fs::write(&empty, b"").unwrap();
    let mut store2 = SessionStore::create_at(empty.clone(), Path::new("/proj/c"), "m").unwrap();
    store2
        .append(&entry_message(Some("hi"), vec![], None, false))
        .unwrap();
    assert_eq!(load(&empty).unwrap().1.len(), 1);

    // Directory path is rejected.
    assert!(SessionStore::create_at(root.clone(), Path::new("/proj/c"), "m").is_err());

    // Bare relative filename (empty parent()) does not fail create_dir_all("").
    let mut bare =
        SessionStore::create_at(PathBuf::from("bare.jsonl"), Path::new("/proj/c"), "m").unwrap();
    bare.path = root.join("bare-unused.jsonl");

    // TOCTOU: file becomes non-empty between create_at and first append.
    let toctou = root.join("toctou.jsonl");
    let mut store3 = SessionStore::create_at(toctou.clone(), Path::new("/proj/c"), "m").unwrap();
    std::fs::write(&toctou, b"{\"type\":\"session\"}\n").unwrap();
    assert!(
        store3
            .append(&entry_message(Some("late"), vec![], None, false))
            .is_err()
    );

    let _ = std::fs::remove_dir_all(&root);
}
