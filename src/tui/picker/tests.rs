//! Tests for [`super`]: the row's strings, the ages, the project column,
//! the filter, the picker's two scopes, and how a row and the hint are
//! drawn.

use std::path::{Path, PathBuf};

use ratatui::text::Line as UiLine;

use super::*;
use crate::agent::session::io::SessionFile;
use crate::tui::history::History;
use crate::tui::picker::sessions::{
    NAME_CAP, NAME_FLOOR, PROJECT_CAP, age, project_label, row_line, stamp_epoch, tail,
};
use crate::tui::theme::TRUECOLOR;

/// 2026-11-25 12:00:00 UTC, as a Unix timestamp.
const NOW: i64 = 1_795_608_000;

/// A session filed as `<stamp>-<id>.jsonl`, with a uuid-shaped `id` that
/// shares nothing with the id in its name (as in a real store).
fn session(id: &str, stamp: &str, title: Option<&str>, preview: Option<&str>) -> SessionFile {
    let mut file = other_file(id, stamp);
    file.title = title.map(Into::into);
    file.preview = preview.map(Into::into);
    file
}

/// A session filed as `<stamp>-<id>.jsonl`, with a uuid-shaped `id` that
/// shares nothing with the id in its name (as in a real store).
fn other_file(id: &str, stamp: &str) -> SessionFile {
    SessionFile {
        id: format!("f3e2d1c0-{id}-4a7b-9c1d-0123456789ab"),
        timestamp: String::new(),
        model: "strata/qwen3.8".into(),
        cwd: "/tmp/x".into(),
        title: None,
        preview: None,
        path: PathBuf::from(format!("/home/u/.gg/sessions/-tmp-x/{stamp}-{id}.jsonl")),
    }
}

fn row(id: &str, title: Option<&str>, preview: Option<&str>) -> SessionRow {
    SessionRow::new(session(id, "20261125-090000", title, preview), NOW)
}

/// The session ids of the rows a filter keeps.
fn ids<'a>(rows: &[&'a Row]) -> Vec<&'a str> {
    rows.iter()
        .map(|row| match row {
            Row::Session(session) => session.id.as_str(),
            Row::Action(_) => "",
        })
        .collect()
}

fn keeps<'a>(rows: &'a [Row], filter: &str) -> Vec<&'a str> {
    ids(&matching(rows, filter))
}

/// What one row's spans say, joined.
fn row_text(line: &UiLine) -> String {
    line.spans
        .iter()
        .map(|span| span.content.to_string())
        .collect::<Vec<_>>()
        .join("")
}

/// The label span: everything after the marker.
fn label(line: &UiLine) -> String {
    line.spans[1].content.to_string()
}

/// The description span: the dim text at the right.
fn description(line: &UiLine) -> String {
    line.spans[3].content.to_string()
}

#[test]
fn a_row_is_its_id_its_name_and_its_age() {
    let titled = row(
        "a1b2c3d4",
        Some("Fix the parser"),
        Some("the parser breaks on…"),
    );
    assert_eq!(
        titled.id, "a1b2c3d4",
        "the id in the file name, not the uuid"
    );
    assert_eq!(
        titled.name, "Fix the parser",
        "a title outranks the preview"
    );
    assert_eq!(titled.age, "3h ago");
    assert_eq!(
        titled.project, None,
        "this project's view shows no directory"
    );

    let untitled = row("a1b2c3d4", None, Some("why does the CLI hang on stdin?"));
    assert_eq!(untitled.name, "why does the CLI hang on stdin?");

    let empty = row("a1b2c3d4", None, None);
    assert_eq!(empty.name, "(no messages)", "a file with nothing in it yet");
    assert_eq!(
        row("a1b2c3d4", Some("  \t "), None).name,
        "(no messages)",
        "a title that is all whitespace names nothing"
    );
}

#[test]
fn a_row_keeps_the_session_it_was_built_from() {
    let session = session("a1b2c3d4", "20261125-090000", Some("Fix the parser"), None);
    let row = SessionRow::new(session.clone(), NOW);
    assert_eq!(row.session.id, session.id);
    assert_eq!(row.session.path, session.path);
    assert_eq!(row.session.model, "strata/qwen3.8");
}

#[test]
fn a_name_is_one_line_and_is_cut_to_the_row() {
    let long_name = "x".repeat(200);
    let long = row("a1b2c3d4", Some(&long_name), None);
    assert_eq!(long.name, long_name, "whole until it is drawn");

    let line = row_line(&TRUECOLOR, &long, false, 80);
    assert_eq!(
        label(&line),
        format!("3h ago a1b2c3d4  {}…", "x".repeat(NAME_CAP - 1)),
        "this project's view gives the name its cap, ellipsis included"
    );
    let left = 80 - 2 - label(&line).chars().count() - 2;
    assert_eq!(
        description(&line),
        "strata/qwen3.8".chars().take(left).collect::<String>(),
        "the model takes the {left} chars the name leaves"
    );

    let split = row("a1b2c3d4", Some("first line\n\tsecond   line"), None);
    assert_eq!(split.name, "first line second line");
}

#[test]
fn a_rows_search_text_is_the_id_the_uuid_the_name_the_preview_the_model_and_the_directory() {
    let row = row(
        "a1b2c3d4",
        Some("Fix the parser"),
        Some("the parser breaks on…"),
    );
    let wrapped = Row::Session(row);
    let text = wrapped.search_text();
    for word in [
        "a1b2c3d4",
        "f3e2d1c0",
        "fix the parser",
        "the parser breaks on…",
        "strata/qwen3.8",
        "/tmp/x",
    ] {
        assert!(
            text.contains(word),
            "{word:?} is in the search text: {text}"
        );
    }
    assert_eq!(text, text.to_lowercase(), "lower-cased");
}

#[test]
fn two_rows_are_the_same_row_when_they_are_the_same_session() {
    let session = session("a1b2c3d4", "20261125-090000", Some("Fix the parser"), None);
    let one = Row::Session(SessionRow::new(session.clone(), NOW));
    let same = Row::Session(SessionRow::new(session.clone(), NOW + 60));
    let other = Row::Session(SessionRow::new(
        other_file("b2c3d4e5", "20261125-090000"),
        NOW,
    ));
    assert!(one.same_row(&same), "the same session, whatever the age");
    assert!(!one.same_row(&other), "another session is another row");
}

#[test]
fn ages_read_minutes_hours_days_then_the_date() {
    let age_of = |stamp: &str| age(Some(stamp), NOW);
    assert_eq!(age_of("20261125-115959"), "just now");
    assert_eq!(
        age_of("20261125-115940"),
        "just now",
        "20s is still just now"
    );
    assert_eq!(age_of("20261125-115900"), "1m ago", "the first minute");
    assert_eq!(age_of("20261125-115500"), "5m ago");
    assert_eq!(
        age_of("20261125-120001"),
        "just now",
        "a future stamp (a clock that moved back)"
    );
    assert_eq!(age_of("20261125-120000"), "just now", "the moment itself");
    assert_eq!(
        age_of("20261125-110001"),
        "59m ago",
        "the last minute of the hour"
    );
    assert_eq!(age_of("20261125-110000"), "1h ago");
    assert_eq!(age_of("20261125-090000"), "3h ago");
    assert_eq!(
        age_of("20261124-120001"),
        "23h ago",
        "the last hour of the day"
    );
    assert_eq!(age_of("20261124-120000"), "1d ago");
    assert_eq!(age_of("20261123-120000"), "2d ago");
    assert_eq!(
        age_of("20261118-120001"),
        "6d ago",
        "the last hour before the cutoff"
    );
    assert_eq!(age_of("20261118-120000"), "2026-11-18", "a week is a date");
    assert_eq!(age_of("20260101-000000"), "2026-01-01");
}

#[test]
fn a_stamp_that_is_not_a_stamp_ages_nothing() {
    assert_eq!(age(None, NOW), "");
    for stamp in [
        "",
        "20261125",
        "20261125-1200",
        "20261325-120000", // month 13
        "20261132-120000", // day 32
        "20261125-246000", // hour 24
        "20261125-120060", // second 60
        "19691125-120000", // before the epoch
        "2026112a-120000", // not all digits
        "20261125120000",  // no hyphen
        "not-a-stamp-at-all",
    ] {
        assert_eq!(age(Some(stamp), NOW), "", "{stamp:?}");
    }
    // The arithmetic is the calendar's, at three known instants.
    assert_eq!(stamp_epoch("19700101-000000"), Some(0));
    assert_eq!(
        stamp_epoch("20000301-000000"),
        Some(951_868_800),
        "across a leap day"
    );
    assert_eq!(stamp_epoch("20261125-120000"), Some(NOW));
}

#[test]
fn a_filter_keeps_the_rows_where_every_word_matches() {
    let rows: [Row; 3] = [
        Row::Session(row(
            "a1b2c3d4",
            Some("Fix the parser"),
            Some("the parser breaks on…"),
        )),
        Row::Session(row("b2c3d4e5", None, Some("write tests for the CLI"))),
        Row::Session(row("c3d4e5f6", None, None)),
    ];
    assert_eq!(
        keeps(&rows, ""),
        vec!["a1b2c3d4", "b2c3d4e5", "c3d4e5f6"],
        "no filter keeps everything, in listing order"
    );
    assert_eq!(keeps(&rows, "  \t ").len(), 3, "whitespace is no filter");

    assert_eq!(keeps(&rows, "PARSER"), vec!["a1b2c3d4"], "case-insensitive");
    assert_eq!(
        keeps(&rows, "breaks"),
        vec!["a1b2c3d4"],
        "a titled row is found by a word of its preview too"
    );
    assert_eq!(
        keeps(&rows, "a1b2"),
        vec!["a1b2c3d4"],
        "the id in the file name"
    );
    assert_eq!(
        keeps(&rows, "f3e2d1c0"),
        vec!["a1b2c3d4", "b2c3d4e5", "c3d4e5f6"],
        "the uuid"
    );
    assert_eq!(
        keeps(&rows, "qwen"),
        vec!["a1b2c3d4", "b2c3d4e5", "c3d4e5f6"],
        "the model"
    );
    assert_eq!(
        keeps(&rows, "the  CLI"),
        vec!["b2c3d4e5"],
        "every word, in any order, any gap"
    );
    assert!(
        keeps(&rows, "parser tests").is_empty(),
        "one word on each row is not both on one"
    );
    assert!(keeps(&rows, "zzz").is_empty());
    assert!(matching(&[], "").is_empty());
}

/// A session in a worktree of a repository under `home`.
fn worktree(id: &str, name: &str, worktree: &str) -> SessionFile {
    let mut session = session(id, "20261125-090000", Some(name), None);
    session.cwd = format!("/home/u/Projects/repo/.worktrees/{worktree}");
    session
}

#[test]
fn a_project_is_its_directory_with_home_as_a_tilde() {
    let home = Path::new("/home/u");
    let label = |cwd: &str| project_label(cwd, Some(home));
    assert_eq!(label("/home/u/proj").as_deref(), Some("~/proj"));
    assert_eq!(label("/home/u").as_deref(), Some("~"), "home itself");
    assert_eq!(
        label("/home/ux/proj").as_deref(),
        Some("/home/ux/proj"),
        "a sibling whose name merely starts with home's"
    );
    assert_eq!(
        label("/srv/app").as_deref(),
        Some("/srv/app"),
        "elsewhere stays absolute"
    );
    assert_eq!(
        project_label("/home/u/proj", None).as_deref(),
        Some("/home/u/proj"),
        "no home known: nothing to abbreviate"
    );
    assert_eq!(
        project_label("", Some(home)),
        None,
        "a header with no directory"
    );
}

#[test]
fn a_long_project_keeps_its_end() {
    let home = Path::new("/home/u");
    // An ordinary worktree of a repository under home fits whole.
    let worktree = "/home/u/Projects/ganggang/.worktrees/feature-x";
    assert_eq!(
        project_label(worktree, Some(home)).as_deref(),
        Some("~/Projects/ganggang/.worktrees/feature-x")
    );

    let at_cap = format!("~/{}", "a".repeat(PROJECT_CAP - 2));
    assert_eq!(at_cap.chars().count(), PROJECT_CAP);
    assert_eq!(
        project_label(
            &format!("/home/u/{}", "a".repeat(PROJECT_CAP - 2)),
            Some(home)
        )
        .as_deref(),
        Some(at_cap.as_str()),
        "exactly the cap is whole"
    );
    assert_eq!(
        project_label(
            &format!("/home/u/{}", "a".repeat(PROJECT_CAP - 1)),
            Some(home)
        )
        .unwrap(),
        format!("…{}", "a".repeat(PROJECT_CAP - 1)),
        "one past it loses its start, and only its start"
    );
}

#[test]
fn an_all_projects_row_shows_the_directory_in_the_model_s_place() {
    let session = worktree("a1b2c3d4", "Fix the parser", "feature-x");
    let home = Path::new("/home/u");
    let here = SessionRow::new(session.clone(), NOW);
    let anywhere = SessionRow::anywhere(session, NOW, Some(home));

    assert_eq!(
        anywhere.project.as_deref(),
        Some("~/Projects/repo/.worktrees/feature-x")
    );
    assert_eq!(
        (
            anywhere.id.as_str(),
            anywhere.name.as_str(),
            anywhere.age.as_str()
        ),
        ("a1b2c3d4", "Fix the parser", "3h ago"),
        "everything else about the row is the same"
    );
    let line = row_line(&TRUECOLOR, &anywhere, false, 80);
    assert_eq!(
        row_text(&line),
        "  3h ago a1b2c3d4  Fix the parser  ~/Projects/repo/.worktrees/feature-x",
        "the directory stands where the model does in this project's view"
    );

    // The directory is among what a filter matches, in both views: a
    // filter typed in one view keeps matching the same rows after Tab.
    let here = [Row::Session(here)];
    let anywhere = [Row::Session(anywhere)];
    assert_eq!(keeps(&here, "projects"), vec!["a1b2c3d4"]);
    assert_eq!(keeps(&anywhere, "PARSER feature-x"), vec!["a1b2c3d4"]);
    assert!(keeps(&anywhere, "main").is_empty());
}

#[test]
fn an_all_projects_row_gives_the_directory_its_room() {
    let session = worktree("a1b2c3d4", &"x".repeat(80), "feature-x");
    let row = SessionRow::anywhere(session, NOW, Some(Path::new("/home/u")));
    let project = row.project.clone().unwrap();
    let before = "3h ago a1b2c3d4  ".chars().count();
    let drawn = |width: usize| row_line(&TRUECOLOR, &row, false, width);

    // 80 columns: the directory is whole, and the name takes the rest of
    // the row, down from its cap.
    let room = 80 - 2 - before;
    let name_cap = room - 2 - project.chars().count();
    assert!(
        name_cap > NAME_FLOOR && name_cap < NAME_CAP,
        "the name gives some ground and keeps more than its floor: {name_cap}"
    );
    let line = drawn(80);
    assert_eq!(description(&line), project, "the directory is whole");
    assert_eq!(
        label(&line),
        format!("3h ago a1b2c3d4  {}…", "x".repeat(name_cap - 1))
    );
    assert_eq!(
        row_text(&line).chars().count(),
        80,
        "the row is exactly full: nothing is left for the renderer to cut"
    );

    // A wide row gives the name its cap back; the directory needs no more.
    let wide = drawn(160);
    assert_eq!(
        label(&wide),
        format!("3h ago a1b2c3d4  {}…", "x".repeat(NAME_CAP - 1))
    );
    assert_eq!(description(&wide), project);

    // A narrow row keeps the name's floor, and the directory keeps as much
    // of its end as the rest of the row holds.
    let narrow = drawn(40);
    let project_room = (40 - 2 - before) - NAME_FLOOR - 2;
    assert_eq!(
        label(&narrow),
        format!("3h ago a1b2c3d4  {}…", "x".repeat(NAME_FLOOR - 1))
    );
    assert_eq!(
        description(&narrow),
        tail(&project, project_room),
        "the worktree's name is the part that survives"
    );
    assert_eq!(row_text(&narrow).chars().count(), 40);

    // Too narrow for a directory at all: the name keeps its floor and
    // the renderer cuts the row, as it cuts every popup row.
    let tiny = drawn(20);
    assert!(row_text(&tiny).chars().count() <= 20, "{}", row_text(&tiny));
}

#[test]
fn a_row_is_age_id_name_then_the_model_dim() {
    let row = row("a1b2c3d4", Some("Fix the parser"), None);
    let off = row_line(&TRUECOLOR, &row, false, 80);
    assert_eq!(
        row_text(&off),
        "  3h ago a1b2c3d4  Fix the parser  strata/qwen3.8"
    );
    let on = row_line(&TRUECOLOR, &row, true, 80);
    assert_eq!(
        row_text(&on).trim_end(),
        "▸ 3h ago a1b2c3d4  Fix the parser  strata/qwen3.8",
        "the marker is all a highlight adds"
    );
    assert_eq!(
        on.spans[0].style.fg,
        Some(TRUECOLOR.accent),
        "the crest marker"
    );
    assert_eq!(on.spans[1].style.bg, Some(TRUECOLOR.selection));
    assert_eq!(
        off.spans[3].style.fg,
        Some(TRUECOLOR.dim),
        "the model is secondary"
    );

    // A row with no readable stamp leads with its id.
    let unstamped = SessionRow::new(
        SessionFile {
            path: PathBuf::from("/x/renamed.jsonl"),
            ..session("a1b2c3d4", "20261125-090000", Some("Fix the parser"), None)
        },
        NOW,
    );
    assert_eq!(unstamped.id, "", "no id in that name either");
    assert_eq!(
        row_text(&row_line(&TRUECOLOR, &unstamped, false, 80)),
        "  Fix the parser  strata/qwen3.8"
    );
}

#[test]
fn a_row_cuts_at_the_width_and_never_wraps() {
    let row = row("a1b2c3d4", Some("Fix the parser"), None);
    let cut = row_line(&TRUECOLOR, &row, true, 16);
    assert_eq!(row_text(&cut), "▸ 3h ago a1b2c3d");
    assert_eq!(row_text(&cut).chars().count(), 16);
    assert_eq!(
        row_text(&row_line(&TRUECOLOR, &row, false, 0)),
        "",
        "width 0 draws nothing"
    );
}

#[test]
fn the_picker_opens_on_every_project_and_narrows_to_this_one() {
    let here = sessions_for_test(3);
    let everywhere = [sessions_for_test(3), elsewhere_for_test(2)].concat();
    let mut pick = Picker::new(here, everywhere, Some(Path::new("/home/u")));

    // It opens on every directory's sessions, each naming its directory,
    // this directory's included.
    assert!(pick.all_directories());
    let uuids: Vec<&str> = pick
        .rows()
        .iter()
        .map(|row| row.session.id.as_str())
        .collect();
    assert_eq!(
        uuids,
        [
            "uuid-of-00000000",
            "uuid-of-00000001",
            "uuid-of-00000002",
            "uuid-any-00000000",
            "uuid-any-00000001",
        ]
    );
    assert_eq!(
        pick.rows()[3].project.as_deref(),
        Some("~/repo/.worktrees/wt00")
    );
    assert!(
        pick.rows().iter().all(|row| row.project.is_some()),
        "every row names its directory"
    );
    pick.selected = 4;

    pick.narrow();
    assert!(!pick.all_directories());
    assert_eq!(pick.rows().len(), 3, "this directory's sessions");
    assert!(
        pick.rows().iter().all(|row| row.project.is_none()),
        "with the model where the directory was"
    );
    assert_eq!(
        pick.selected, 0,
        "a new list: the highlight returns to the top"
    );

    pick.widen();
    assert!(pick.all_directories());
    assert_eq!(pick.rows().len(), 5);
    assert_eq!(pick.selected, 0);
}

#[test]
fn the_hint_names_the_keys_the_scope_and_the_place_in_a_long_list() {
    assert_eq!(
        row_text(&hint_row(&TRUECOLOR, 80, Some(false), None)),
        " ↑↓ move · type filters · Tab: all directories · Enter picks · Esc clears"
    );
    assert_eq!(
        row_text(&hint_row(&TRUECOLOR, 80, Some(true), None)),
        " ↑↓ move · type filters · Tab: this directory · Enter picks · Esc clears",
        "the hint names the scope Tab switches TO"
    );
    let long = row_text(&hint_row(&TRUECOLOR, 80, Some(false), Some((2, 57))));
    assert!(long.starts_with(" ↑↓ move 3/57 · "), "{long}");
    assert!(long.ends_with("Esc clears"), "{long}");
    assert!(
        long.chars().count() <= 80,
        "it all fits an 80-column row: {long}"
    );
    let three_digits = row_text(&hint_row(&TRUECOLOR, 80, Some(false), Some((104, 230))));
    assert!(
        three_digits.starts_with(" ↑↓ move 105/230 · "),
        "{three_digits}"
    );
    assert!(
        three_digits.chars().count() <= 80,
        "a three-digit total still fits: {three_digits}"
    );
    assert_eq!(
        row_text(&hint_row(&TRUECOLOR, 10, Some(false), Some((2, 57)))),
        " ↑↓ move 3",
        "narrow: the keys come first, the rest is cut"
    );
    assert_eq!(row_text(&hint_row(&TRUECOLOR, 0, Some(true), None)), "");
}

/// The panel's one row of explanation when the list has nothing to
/// show: the reason as the state words it, with one leading space, in the
/// hint's colour, cut to the row.
#[test]
fn an_empty_list_gets_its_own_row_of_explanation() {
    let all = empty_row(&TRUECOLOR, "no sessions yet", 80);
    assert_eq!(row_text(&all), " no sessions yet", "the reason, as given");
    assert_eq!(
        all.spans[0].style.fg,
        Some(TRUECOLOR.dim),
        "the panel's hint colour"
    );
    assert_eq!(
        all.spans[0].style.bg,
        Some(TRUECOLOR.surface_alt),
        "on the panel's surface"
    );
    assert_eq!(
        row_text(&empty_row(&TRUECOLOR, "no sessions in this directory", 6)),
        " no se",
        "cut to the row's width"
    );
    assert_eq!(row_text(&empty_row(&TRUECOLOR, "no sessions yet", 0)), "");
}

/// The panel's last row after a failed open: the error itself, in the
/// warning's colour on the panel's surface, cut to the row.
#[test]
fn a_failed_open_reads_its_error_from_the_panel_s_last_row() {
    let line = error_row(&TRUECOLOR, "no agent here", 80);
    assert_eq!(row_text(&line), "⚠ no agent here");
    assert_eq!(
        line.spans[0].style.fg,
        Some(TRUECOLOR.warn),
        "the warning's colour"
    );
    assert_eq!(
        line.spans[0].style.bg,
        Some(TRUECOLOR.surface_alt),
        "on the panel's surface"
    );
    assert_eq!(row_text(&error_row(&TRUECOLOR, "abcdefghij", 5)), "⚠ abc");
    assert_eq!(row_text(&error_row(&TRUECOLOR, "boom", 0)), "");
}

// The seams the other test modules build a picker with.
/// A session for the test seams: filed as `<stamp>-<id>.jsonl`, titled
/// `{name}{i:02}`, its uuid prefixed so one listing's rows can be told
/// from another's.
pub(in crate::tui) fn filed_for_test(i: usize, uuid_prefix: &str, name: &str) -> SessionFile {
    let id = format!("{i:08x}");
    SessionFile {
        id: format!("{uuid_prefix}-{id}"),
        timestamp: String::new(),
        model: "m".into(),
        cwd: "/tmp/x".into(),
        title: Some(format!("{name}{i:02}")),
        preview: None,
        path: std::path::PathBuf::from(format!("/s/20261125-120000-{id}.jsonl")),
    }
}

/// Test seam: this project's `n` sessions `s00`.. in a loaded listing
/// (see [`listing_of`]), and the sessions themselves.
pub(in crate::tui) fn listing_for_test(
    n: usize,
) -> (crate::tui::picker::SessionListing, Vec<SessionFile>) {
    let sessions = sessions_for_test(n);
    (listing_of(sessions.clone()), sessions)
}

/// Test seam: a loaded listing of `sessions`, with `/s` as the
/// directory this directory's sessions are filed under.
pub(in crate::tui) fn listing_of(sessions: Vec<SessionFile>) -> crate::tui::picker::SessionListing {
    let mut listing = crate::tui::picker::SessionListing::new(std::path::Path::new("/s"));
    listing.dir = Some(std::path::PathBuf::from("/s"));
    listing.home = Some(std::path::PathBuf::from("/home/u"));
    listing.loaded = true;
    listing.everywhere = sessions;
    listing
}

/// Test seam: this project's `n` sessions `s00`.., as they are filed
/// under `/s`.
pub(in crate::tui) fn sessions_for_test(n: usize) -> Vec<SessionFile> {
    (0..n).map(|i| filed_for_test(i, "uuid-of", "s")).collect()
}

/// Test seam: the picker on this project's `n` sessions `s00`.., each
/// aged against a fixed instant, showing this project's rows only (what
/// Tab narrows it to). No test reads the clock or the disk.
pub(in crate::tui) fn pick_for_test(n: usize) -> Picker {
    Picker::for_test(
        (0..n)
            .map(|i| SessionRow::new(filed_for_test(i, "uuid-of", "s"), 1_795_608_000 + 3_600))
            .collect(),
    )
}

/// Test seam: `n` sessions of other projects, `a00`.., one worktree each
/// under `/home/u/repo` and filed under `/elsewhere`, to open beside
/// [`sessions_for_test`] for every project's rows.
pub(in crate::tui) fn elsewhere_for_test(n: usize) -> Vec<SessionFile> {
    (0..n)
        .map(|i| {
            let id = format!("{i:08x}");
            let mut session = filed_for_test(i, "uuid-any", "a");
            session.cwd = format!("/home/u/repo/.worktrees/wt{i:02}");
            session.path =
                std::path::PathBuf::from(format!("/elsewhere/20261125-120000-{id}.jsonl"));
            session
        })
        .collect()
}

// The state half of the list, tested without a terminal: what the
// trigger's text opens, what the cache's listing does to it, and what
// its keys do.
mod state_tests {
    use super::*;
    use crate::tui::view::State;

    /// A view for the state's tests: no environment read is a test read.
    fn view() -> State {
        State::with_palette(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(Vec::new()),
            History::default(),
            TRUECOLOR,
        )
    }

    /// The list open on `n` sessions, all filed under `/s`, which is
    /// also where this directory's sessions are filed (so both scopes
    /// show them): the state the loop's first frame reaches after the
    /// first scan's listing is stored. The listing is in place before the
    /// list opens, as in the loop.
    fn list_open_on(state: &mut State, n: usize) {
        let (listing, _) = listing_for_test(n);
        state.sessions = listing;
        state.editor.set_text("/session");
        state.sync_picker();
    }

    fn names(state: &State) -> Vec<String> {
        state
            .picker_matches()
            .iter()
            .map(|row| match row {
                Row::Session(session) => session.name.clone(),
                Row::Action(action) => match action {
                    Action::CopyPath => "copy path".to_string(),
                    Action::Info => "info".to_string(),
                },
            })
            .collect()
    }

    fn highlighted(state: &State) -> usize {
        state.picker.as_ref().map_or(0, |pick| pick.selected)
    }

    /// Setting the text to `/session` against a cached listing of three
    /// sessions, one of them the current one, opens the list with the
    /// other two, newest first, and the top row highlighted.
    #[test]
    fn typing_the_command_opens_the_list_from_the_cache_dropping_the_session_in_use() {
        let mut state = view();
        let current = filed_for_test(0, "uuid-of", "s");
        state.session_path = Some(current.path.clone());
        state.sessions = SessionListing::new(std::path::Path::new("/s"));
        state.sessions.dir = Some(std::path::PathBuf::from("/s"));
        state.sessions.loaded = true;
        state.sessions.everywhere = vec![
            filed_for_test(2, "uuid-of", "s"),
            current.clone(),
            filed_for_test(1, "uuid-of", "s"),
        ];
        state.editor.set_text("/session");
        state.sync_picker();
        assert_eq!(
            names(&state),
            ["s02", "s01"],
            "the other two, newest first, the session in use not listed"
        );
        assert_eq!(highlighted(&state), 0, "the top row");
    }

    /// The highlight moves with the arrows, returns to the top and its
    /// error clears when the filter changes; a trailing space is not a
    /// new filter, so the highlight stays.
    #[test]
    fn a_changed_filter_puts_the_highlight_back_on_the_top_row_and_a_trailing_space_is_not_a_change()
     {
        let mut state = view();
        list_open_on(&mut state, 3);
        state.picker_next();
        assert_eq!(highlighted(&state), 1);
        state.editor.set_text("/session s02");
        state.sync_picker();
        assert_eq!(
            highlighted(&state),
            0,
            "a changed filter: the top row again"
        );
        state.editor.set_text("/session s02 ");
        state.sync_picker();
        assert_eq!(
            highlighted(&state),
            0,
            "a trailing space is not a new filter"
        );
        state.pick_failed("boom");
        assert!(state.picker.as_ref().unwrap().error.is_some());
        state.editor.set_text("/session s01");
        state.sync_picker();
        assert!(
            state.picker.as_ref().unwrap().error.is_none(),
            "and a changed filter clears the last failed open"
        );
    }

    /// Text that is not a trigger closes the list and discards its
    /// scope, highlight and error. `close_picker` and `interrupt()`
    /// clear the composer; `interrupt()` returns `Cancel::PickerClosed`.
    #[test]
    fn text_that_is_not_a_trigger_closes_the_list_and_discards_it() {
        let mut state = view();
        list_open_on(&mut state, 3);
        state.picker_next();
        state.session_scope_this_directory();
        state.pick_failed("boom");
        state.editor.set_text("just words");
        state.sync_picker();
        assert!(
            !state.picker_open(),
            "a text with no trigger closes the list"
        );
        assert!(state.picker_matches().is_empty());

        list_open_on(&mut state, 3);
        state.close_picker();
        assert!(!state.picker_open());
        assert_eq!(state.editor.text(), "", "the composer is cleared");

        list_open_on(&mut state, 3);
        assert_eq!(state.interrupt(), crate::tui::view::Cancel::PickerClosed);
        assert!(!state.picker_open());
        assert_eq!(state.editor.text(), "", "and so is the composer");
    }

    /// A listing that arrives while the list is open is its new rows:
    /// the highlight stays on the session it stood on, by its path,
    /// wherever it now stands, and falls back to the first row when that
    /// session is gone.
    #[test]
    fn a_listing_that_arrives_keeps_the_highlight_on_the_same_session() {
        let mut state = view();
        list_open_on(&mut state, 3);
        state.picker_next();
        state.picker_next();
        assert_eq!(highlighted(&state), 2, "standing on s02");

        // Another directory's session joins the listing: s02 is still
        // there, so the highlight is still on it.
        let mut listing = sessions_for_test(3);
        listing.extend(elsewhere_for_test(1));
        state.sessions_listed(listing, Vec::new());
        assert!(state.picker_open(), "the list stays open on the new rows");
        assert_eq!(names(&state), ["s00", "s01", "s02", "a00"]);
        assert_eq!(highlighted(&state), 2, "the session it stood on, by path");

        // It is gone from the listing: the first row again.
        state.editor.set_text("");
        state.sync_picker();
        state.sessions_listed(elsewhere_for_test(1), Vec::new());
        state.editor.set_text("/session");
        state.sync_picker();
        assert_eq!(names(&state), ["a00"]);
        assert_eq!(
            highlighted(&state),
            0,
            "the session is gone: the first row again"
        );
    }

    /// Before the first listing the panel's row says it; with
    /// `--no-session` it says that instead, and no scan is asked for.
    #[test]
    fn before_the_first_listing_the_panel_says_what_it_says() {
        let mut state = view();
        // A store to scan, wherever the test itself runs.
        state.sessions.dir = Some(PathBuf::from("/s"));
        state.editor.set_text("/session");
        state.sync_picker();
        assert!(
            state.picker_open(),
            "the list opens even when the cache is empty"
        );
        assert!(state.session_scope_empty(), "no rows yet");
        assert_eq!(
            state.session_empty_reason(),
            "reading sessions…",
            "until the first listing arrives"
        );
        assert!(
            state.session_scan_ready(),
            "a scan is owed: the panel is where the reading row is said"
        );
        state.session_scan_started();
        assert!(
            !state.session_scan_ready(),
            "in flight: the same request is kept until it returns"
        );

        let mut disabled = view();
        disabled.sessions_disabled = true;
        disabled.editor.set_text("/session");
        disabled.sync_picker();
        assert!(disabled.picker_open());
        assert_eq!(
            disabled.session_empty_reason(),
            "sessions are disabled for --no-session runs"
        );
        assert!(
            !disabled.session_scan_ready(),
            "--no-session runs have no store to scan"
        );
    }

    /// The panel's reasons for an empty list, in the state's words: a
    /// missing HOME, a store with nothing but the session in use, a
    /// directory with no sessions of its own, and a filter that keeps no
    /// row, in the composer's words.
    #[test]
    fn the_state_words_why_the_list_is_empty() {
        let mut state = view();
        // No HOME: said before anything else, since no scan can run.
        state.editor.set_text("/session");
        state.sync_picker();
        state.sessions.dir = None;
        assert_eq!(
            state.session_empty_reason(),
            "no HOME: nowhere to look for sessions"
        );

        // Only the session in use: nothing to pick from.
        state.editor.set_text("");
        state.sync_picker();
        let (mut listing, sessions) = listing_for_test(2);
        listing.everywhere = vec![sessions[1].clone()];
        state.session_path = Some(sessions[1].path.clone());
        state.sessions = listing;
        state.editor.set_text("/session");
        state.sync_picker();
        assert_eq!(state.session_empty_reason(), "no sessions yet");

        // Sessions elsewhere only: this directory's view is empty.
        state.editor.set_text("");
        state.sync_picker();
        state.session_path = None;
        state.sessions_listed(elsewhere_for_test(2), Vec::new());
        state.editor.set_text("/session");
        state.sync_picker();
        assert_eq!(names(&state).len(), 2, "every directory's");
        state.session_scope_this_directory();
        assert_eq!(
            state.session_empty_reason(),
            "no sessions in this directory"
        );

        // A filter that keeps no row.
        state.session_scope_all_directories();
        state.editor.set_text("/session  fix  ");
        state.sync_picker();
        assert_eq!(
            state.session_empty_reason(),
            "no session matches \"fix\"",
            "the filter, trimmed"
        );
    }

    /// The scan's bookkeeping: a request made while a scan is in flight
    /// is kept, `session_scan_ready` is false until that scan's result
    /// is stored, then true once.
    #[test]
    fn a_request_while_a_scan_is_in_flight_is_kept_until_it_returns() {
        let mut state = view();
        assert!(!state.session_scan_ready());
        state.request_session_scan();
        assert!(state.session_scan_ready());
        state.session_scan_started();
        assert!(!state.session_scan_ready(), "in flight: not ready");
        state.request_session_scan();
        assert!(
            !state.session_scan_ready(),
            "a request made in flight is kept, still not ready"
        );
        state.sessions_listed(Vec::new(), Vec::new());
        assert!(
            state.session_scan_ready(),
            "the kept request runs when the scan returns"
        );
        state.session_scan_started();
        state.sessions_listed(Vec::new(), Vec::new());
        assert!(!state.session_scan_ready(), "and then: nothing is owed");
    }

    /// Warnings: pushed when they differ from the previous scan's; the
    /// same warnings again push nothing.
    #[test]
    fn a_scan_says_its_warnings_when_they_differ_and_says_them_not_twice() {
        let mut state = view();
        let warnings = vec!["a file: read failed".to_string()];
        state.sessions_listed(Vec::new(), warnings.clone());
        assert_eq!(
            state.line_texts_for_test(),
            vec!["⚠ a file: read failed"],
            "the new warnings reach the transcript"
        );
        state.sessions_listed(Vec::new(), warnings.clone());
        assert_eq!(
            state.line_texts_for_test(),
            vec!["⚠ a file: read failed"],
            "the same warnings are not said twice"
        );
        state.sessions_listed(Vec::new(), vec!["another file: read failed".to_string()]);
        assert_eq!(
            state.line_texts_for_test(),
            vec!["⚠ a file: read failed", "⚠ another file: read failed"],
            "a changed warning is the scan's to say"
        );
    }

    /// A filter keeps the matching action rows above the matching
    /// sessions, in `copy path`, then `info` order: the label is the
    /// row's whole search text, and an empty filter keeps no action row
    /// at all.
    #[test]
    fn a_filter_keeps_the_matching_action_rows_above_the_sessions() {
        let mut state = view();
        let (listing, _sessions) = listing_for_test(0);
        state.sessions = listing;
        state.editor.set_text("/session");
        state.sync_picker();
        assert!(
            names(&state).is_empty(),
            "no filter, no sessions: no action rows"
        );

        state.editor.set_text("/session copy");
        state.sync_picker();
        assert_eq!(
            names(&state),
            ["copy path"],
            "the label's word keeps its row, above the matching sessions"
        );
        assert_eq!(highlighted(&state), 0, "and the top row is the new top row");

        state.editor.set_text("/session path");
        state.sync_picker();
        assert_eq!(
            names(&state),
            ["copy path"],
            "a word of the other label is not this one"
        );

        state.editor.set_text("/session info");
        state.sync_picker();
        assert_eq!(names(&state), ["info"]);

        state.editor.set_text("/session cop");
        state.sync_picker();
        assert_eq!(
            names(&state),
            ["copy path"],
            "the label is the row's whole search text: no session's text has cop"
        );

        state.editor.set_text("/session i");
        state.sync_picker();
        assert_eq!(
            names(&state),
            ["info"],
            "i matches no session row: no copy path either"
        );

        state.editor.set_text("/session zzz");
        state.sync_picker();
        assert!(
            names(&state).is_empty(),
            "a filter that matches nothing: no rows at all"
        );
    }

    /// `picker_action()` on an action row is the action itself.
    #[test]
    fn the_action_rows_act_on_their_actions() {
        let mut state = view();
        let (listing, _sessions) = listing_for_test(0);
        let mut with_copy = listing;
        let mut copy_session = sessions_for_test(1)[0].clone();
        copy_session.title = Some("copy the tests".into());
        with_copy.everywhere = vec![copy_session.clone()];
        state.sessions = with_copy;
        state.editor.set_text("/session");
        state.sync_picker();
        state.editor.set_text("/session copy");
        state.sync_picker();
        assert_eq!(
            state.picker_action(),
            Some(Pick::CopyPath),
            "the top row is copy path"
        );
        state.picker_next();
        assert_eq!(
            state.picker_action(),
            Some(Pick::Open(copy_session.clone())),
            "the session the filter keeps is the next row"
        );
        state.picker_next();
        assert_eq!(
            state.picker_action(),
            Some(Pick::Open(copy_session.clone())),
            "it stops at the last row the filter keeps"
        );
        state.picker_next();
        assert_eq!(
            state.picker_action(),
            Some(Pick::Open(copy_session)),
            "and a further key moves it nowhere"
        );
    }

    /// A failed copy keeps the composer text and the list, and says the
    /// error in the panel's last row.
    #[test]
    fn a_failed_copy_keeps_the_text_and_the_list_and_sets_the_error() {
        let mut state = view();
        list_open_on(&mut state, 1);
        state.editor.set_text("/session copy");
        state.sync_picker();
        assert_eq!(state.picker_action(), Some(Pick::CopyPath));
        state.pick_failed("cannot write to /dev/tty: no such device");
        assert!(state.picker_open(), "the list stays");
        assert_eq!(state.editor.text(), "/session copy", "the text stays");
        assert_eq!(
            state.picker.as_ref().unwrap().error.as_deref(),
            Some("cannot write to /dev/tty: no such device"),
            "the panel's last row says why"
        );
    }

    /// `new_session` and `set_directory` close the list or re-file it and
    /// ask for a scan: the new session's file is the newest, and the new
    /// directory is where this directory's rows are filed. The listing
    /// stays until the scan replaces it.
    #[test]
    fn a_new_session_and_a_new_directory_close_the_list_and_ask_for_a_scan() {
        let mut state = view();
        list_open_on(&mut state, 3);
        state.new_session(None, false);
        assert!(!state.picker_open(), "the list is closed");
        assert!(state.session_scan_ready(), "a scan is owed");

        state.session_scan_started();
        state.sessions_listed(sessions_for_test(3), Vec::new());
        assert!(!state.session_scan_ready(), "nothing is owed now");
        state.set_directory(std::path::Path::new("/elsewhere"));
        assert!(state.sessions.wanted, "the new directory is owed a scan");
        assert_eq!(
            state.sessions.dir,
            crate::agent::session::io::sessions_dir(std::path::Path::new("/elsewhere")),
            "its rows are filed under the new directory"
        );
        assert_eq!(
            state.sessions.everywhere.len(),
            3,
            "the listing stays until the scan replaces it"
        );
    }
}

#[cfg(test)]
mod trigger_tests {
    use super::*;

    /// The trigger, as `(list, filter)` where one is.
    fn of(text: &str) -> Option<(List, String)> {
        trigger(text).map(|t| (t.list, t.filter))
    }

    #[test]
    fn the_session_command_opens_the_session_list_with_the_rest_as_its_filter() {
        assert_eq!(
            of("/session"),
            Some((List::Sessions, String::new())),
            "the bare command: an empty filter"
        );
        assert_eq!(
            of("/session copy"),
            Some((List::Sessions, "copy".into())),
            "the action's word is a filter like any other"
        );
        assert_eq!(
            of("/session glm fix"),
            Some((List::Sessions, "glm fix".into())),
            "every word after the command is the filter"
        );
        assert_eq!(
            of("/session "),
            Some((List::Sessions, String::new())),
            "trailing whitespace is no filter"
        );
        for text in [
            "/sessionx",
            "/resum",
            "/resumex",
            "/resume:x",
            "/resume",
            "resume",
            "hello there",
            "/model",
        ] {
            assert_eq!(of(text), None, "{text:?} opens nothing");
        }
        assert_eq!(of(""), None, "an empty composer opens nothing");
    }
}
