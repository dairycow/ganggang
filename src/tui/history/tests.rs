//! History: what is remembered, how a browse walks it, what the file looks
//! like, and when an arrow key means history at all.

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// History that never touches a disk: the same rules, one process.
fn history() -> History {
    History::default()
}

#[test]
fn submissions_are_remembered_newest_last() {
    let mut h = history();
    h.record("first");
    h.record("  second  ");
    h.record("third\nwith a newline");
    assert_eq!(
        h.entries(),
        ["first", "second", "third\nwith a newline"],
        "surrounding whitespace goes, interior stays"
    );
}

#[test]
fn blank_and_immediate_repeats_are_not_entries() {
    let mut h = history();
    h.record("   ");
    h.record("\n");
    assert!(h.entries().is_empty(), "nothing was submitted");
    h.record("same");
    h.record("same");
    h.record("  same  ");
    assert_eq!(
        h.entries(),
        ["same"],
        "a repeat of the last entry is one entry"
    );
    h.record("other");
    h.record("same");
    assert_eq!(
        h.entries(),
        ["same", "other", "same"],
        "not the LAST entry, so it is an entry again"
    );
}

#[test]
fn the_buffer_caps_at_the_newest_five_hundred() {
    let mut h = history();
    for n in 0..520 {
        h.record(&format!("prompt {n}"));
    }
    assert_eq!(h.entries().len(), 500);
    assert_eq!(h.entries().first().unwrap(), "prompt 20");
    assert_eq!(h.entries().last().unwrap(), "prompt 519");
}

#[test]
fn up_walks_back_and_stops_at_the_oldest() {
    let mut h = history();
    h.record("one");
    h.record("two");
    h.record("three");

    assert_eq!(h.prev("").as_deref(), Some("three"));
    assert!(h.browsing());
    assert_eq!(h.prev("three").as_deref(), Some("two"));
    assert_eq!(h.prev("two").as_deref(), Some("one"));
    assert_eq!(h.prev("one"), None, "the oldest is where walking stops");
    assert!(h.browsing(), "and the composer keeps showing it");
    // Pressing on at the oldest changes nothing — including a composer the
    // user has since edited, which is not written over.
    assert_eq!(h.prev("one, edited").as_deref(), None);
    assert_eq!(h.next().as_deref(), Some("two"), "and down walks on");
}

#[test]
fn down_walks_forward_to_the_draft_and_ends_browsing() {
    let mut h = history();
    h.record("one");
    h.record("two");
    assert_eq!(h.prev("draft text").as_deref(), Some("two"));
    assert_eq!(h.prev("two").as_deref(), Some("one"));
    assert_eq!(h.next().as_deref(), Some("two"));
    assert_eq!(h.next().as_deref(), Some("draft text"));
    assert!(
        !h.browsing(),
        "past the newest the composer is the user's again"
    );
    assert_eq!(h.next(), None, "and there is nothing further to walk to");
    assert_eq!(
        h.prev("").as_deref(),
        Some("two"),
        "the next ↑ starts over from the newest"
    );
}

#[test]
fn the_first_up_saves_what_was_being_typed() {
    let mut h = history();
    h.record("older");
    assert_eq!(h.prev("half a thought").as_deref(), Some("older"));
    assert_eq!(h.draft(), Some("half a thought"));
    // Down past the newest brings the draft back rather than losing it.
    assert_eq!(h.next().as_deref(), Some("half a thought"));
}

#[test]
fn an_empty_composer_is_a_blank_draft() {
    let mut h = history();
    h.record("one");
    assert_eq!(h.prev("").as_deref(), Some("one"));
    assert_eq!(h.draft(), Some(""));
    assert_eq!(
        h.next().as_deref(),
        Some(""),
        "and down empties the composer"
    );
    assert!(!h.browsing());
}

#[test]
fn browsing_survives_an_empty_history_and_reset_ends_it() {
    let mut h = history();
    assert_eq!(h.prev("text"), None, "nothing to recall");
    h.record("one");
    assert_eq!(h.prev("text").as_deref(), Some("one"));
    h.reset();
    assert!(!h.browsing());
    assert_eq!(h.draft(), None);
    // A submission ends browsing too, including one that adds no entry:
    // the recalled text sent is the entry that was already there.
    h.prev("other");
    h.record("one");
    assert!(!h.browsing());
    assert_eq!(h.entries(), ["one"], "and the repeat is still no entry");
}

#[test]
fn a_recalled_multiline_prompt_is_still_walkable() {
    let mut h = history();
    h.record("one");
    h.record("two\nlines\nof text");
    assert_eq!(h.prev("").as_deref(), Some("two\nlines\nof text"));
    assert_eq!(h.prev("two\nlines\nof text").as_deref(), Some("one"));
    assert_eq!(h.next().as_deref(), Some("two\nlines\nof text"));
}

// ------------------------------------------------------------- the file --

/// The file's shape: one JSON string per line, newest last, private mode.
#[test]
fn the_file_round_trips_and_stays_the_users_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    {
        let mut h = History::at(path.clone());
        h.record("plain");
        h.record("two\nlines");
        h.record("héllo — \"quoted\"");
    }
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw.lines().count(), 3, "one line per entry");
    assert_eq!(raw.lines().next().unwrap(), "\"plain\"");

    let mut reloaded = History::at(path.clone());
    assert_eq!(
        reloaded.entries(),
        ["plain", "two\nlines", "héllo — \"quoted\""],
        "a reload keeps the order and the newlines"
    );
    assert_eq!(reloaded.warning(), None);
    // A reload is a fresh start: it does not browse.
    assert_eq!(reloaded.prev("").as_deref(), Some("héllo — \"quoted\""));

    let mode = |path: &PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600, "the prompts are the user's alone");

    // Appending keeps that mode even when the file was left loose.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    reloaded.record("next");
    assert_eq!(mode(&path), 0o600);
    // Reading tightens it too.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    History::at(path.clone());
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn history_appears_without_the_directory_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".gg").join("history.jsonl");
    let mut h = History::at(path.clone());
    h.record("first of the first run");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\"first of the first run\"\n"
    );
}

#[test]
fn malformed_lines_are_skipped_and_named_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    std::fs::write(
        &path,
        "\"kept\"\nnot json\n{\"a\":1}\n\"a\" \"b\"\n42\n\n\"   \"\n\"kept too\"\n",
    )
    .unwrap();
    let h = History::at(path.clone());
    assert_eq!(h.entries(), ["kept", "kept too"]);
    let warning = h.warning().expect("a skipped line is reported");
    assert!(warning.contains("6 line(s)"), "{warning}");
    assert!(warning.contains("history.jsonl"), "{warning}");
}

#[test]
fn a_load_keeps_only_the_newest_five_hundred_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let raw: String = (0..600).map(|n| format!("\"prompt {n}\"\n")).collect();
    std::fs::write(&path, raw).unwrap();
    let h = History::at(path.clone());
    assert_eq!(h.entries().len(), 500);
    assert_eq!(h.entries().first().unwrap(), "prompt 100");
    assert_eq!(h.entries().last().unwrap(), "prompt 599");
}

/// Past the compaction threshold a record rewrites the file compactly: the
/// file stops growing, the oldest lines go, and no temp file is left.
#[test]
fn a_long_file_is_rewritten_compactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let raw: String = (0..1001).map(|n| format!("\"prompt {n}\"\n")).collect();
    std::fs::write(&path, raw).unwrap();
    let mut h = History::at(path.clone());
    assert_eq!(
        h.entries().len(),
        500,
        "the load already dropped the oldest"
    );
    h.record("one more");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after.lines().count(), 500, "the cap, rewritten whole");
    assert_eq!(after.lines().last().unwrap(), "\"one more\"");
    assert_eq!(after.lines().next().unwrap(), "\"prompt 502\"");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "the temp file became the history file"
    );
    // And the rewrite left the counter right: the next record is an append
    // again, not another whole rewrite.
    h.record("another");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 501);
}

/// A file gg cannot write is not a failure: the history still works, this
/// process only. (`no-such-name` is a file, so the parent directory the
/// append would create cannot be made.)
#[test]
fn an_unwritable_file_leaves_the_history_working() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("no-such-name"), "not a directory").unwrap();
    let path = dir.path().join("no-such-name").join("history.jsonl");
    let mut h = History::at(path);
    h.record("kept in memory");
    assert_eq!(h.entries(), ["kept in memory"]);
    assert_eq!(h.prev("").as_deref(), Some("kept in memory"));
}

// ------------------------------------------------------- the arrow rule --

#[test]
fn the_popup_owns_the_arrows_unless_a_browse_is_under_way() {
    assert_eq!(
        history_for(true, true, false, 1),
        Arrow::SuggestPrev,
        "even with a single-line composer"
    );
    assert_eq!(
        history_for(false, true, true, 3),
        Arrow::Next,
        "but a browse in progress keeps them, a three-line entry included"
    );
}

#[test]
fn browsing_owns_the_arrows_whatever_the_recalled_text_looks_like() {
    // A three-line recalled prompt is still walked, not edited by line.
    assert_eq!(history_for(true, false, true, 3), Arrow::Prev);
    assert_eq!(history_for(false, false, true, 3), Arrow::Next);
    // So is a recalled command word: the suggestion list it opens over the
    // composer does not take the arrows from the browse.
    assert_eq!(history_for(true, true, true, 1), Arrow::Prev);
}

#[test]
fn a_single_line_or_empty_composer_starts_a_browse_on_up() {
    assert_eq!(history_for(true, false, false, 0), Arrow::Prev);
    assert_eq!(history_for(true, false, false, 1), Arrow::Prev);
    assert_eq!(
        history_for(false, false, false, 0),
        Arrow::Nothing,
        "down has nothing to walk before a browse starts"
    );
    assert_eq!(history_for(false, false, false, 1), Arrow::Nothing);
}

#[test]
fn a_multiline_composer_keeps_its_line_cursor() {
    assert_eq!(history_for(true, false, false, 2), Arrow::LineUp);
    assert_eq!(history_for(false, false, false, 5), Arrow::LineDown);
}
