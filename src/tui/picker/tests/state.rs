//! The state half of the list, tested without a terminal: what the
//! trigger's text opens, what the cache's listing does to it, and what
//! its keys do.

use super::*;
use crate::tui::view::State;

/// A view for the state's tests: no environment read is a test read.
pub(super) fn view() -> State {
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

pub(super) fn names(state: &State) -> Vec<String> {
    state
        .picker_matches()
        .iter()
        .map(|row| match row {
            Row::Session(session) => session.name.clone(),
            Row::Action(action) => match action {
                Action::CopyPath => "copy path".to_string(),
                Action::Info => "info".to_string(),
            },
            Row::Model { suggestion, .. } => suggestion.label.clone(),
            Row::Skill { suggestion, .. } => suggestion.label.clone(),
        })
        .collect()
}

pub(super) fn highlighted(state: &State) -> usize {
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
fn a_changed_filter_puts_the_highlight_back_on_the_top_row_and_a_trailing_space_is_not_a_change() {
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

/// The mouse wheel moves the highlight by its notch, up or down, and
/// stops at the ends like the arrows do.
#[test]
fn a_wheel_notch_moves_the_highlight_and_stops_at_the_ends() {
    let mut state = view();
    list_open_on(&mut state, 3);
    state.picker_scroll(false, 2);
    assert_eq!(highlighted(&state), 2, "two rows down");
    state.picker_scroll(false, 3);
    assert_eq!(highlighted(&state), 2, "the last row holds");
    state.picker_scroll(true, 5);
    assert_eq!(highlighted(&state), 0, "the top row holds");
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

/// A run in flight holds up neither the list nor its one-line actions:
/// `/session copy` still highlights `copy path`, the list stays, and the
/// copy writes the session's path.
#[test]
fn the_copy_action_works_while_a_run_is_in_flight() {
    let mut state = view();
    list_open_on(&mut state, 1);
    state.begin_run();
    state.editor.set_text("/session copy");
    state.sync_picker();
    assert!(state.busy, "the run is in flight");
    assert_eq!(state.picker_action(), Some(Pick::CopyPath));
    assert!(state.picker_open(), "the list stays open");

    state.session_path = Some(std::path::PathBuf::from("/s/a1b2c3d4.jsonl"));
    let copied = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = copied.clone();
    crate::tui::commands::copy_session_path(&mut state, |text| {
        recorded.lock().unwrap().push(text.to_string());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        *copied.lock().unwrap(),
        ["/s/a1b2c3d4.jsonl"],
        "the copy wrote the path while the run was in flight"
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
    state.pick_failed("cannot copy: no clipboard device");
    assert!(state.picker_open(), "the list stays");
    assert_eq!(state.editor.text(), "/session copy", "the text stays");
    assert_eq!(
        state.picker.as_ref().unwrap().error.as_deref(),
        Some("cannot copy: no clipboard device"),
        "the panel's last row says why"
    );
}

/// The `/model` list's own rules, beside the sessions'.
/// The trigger opens it on the bare word, and the rest, trimmed, is
/// its filter: nothing else is a model trigger.
#[test]
fn the_model_trigger_is_the_bare_word_and_its_filter() {
    let none = view();
    assert!(!none.picker_open(), "no text: no list");
    let mut state = view();
    state.editor.set_text("/model");
    state.sync_picker();
    assert!(state.picker_open(), "the bare word opens the list");
    assert_eq!(state.picker.as_ref().unwrap().list, List::Models);
    assert!(state.picker.as_ref().unwrap().filter.is_empty());
    state.editor.set_text("/model  glm ");
    state.sync_picker();
    assert_eq!(
        state.picker.as_ref().unwrap().filter,
        "glm",
        "the rest, trimmed, is the filter"
    );
    state.editor.set_text("/modelx");
    state.sync_picker();
    assert!(!state.picker_open(), "a different word is not a trigger");
}

/// A model row's strings, as the list builds them: the
/// `provider/model` label, the completion, and the description's
/// context window, status, key state and active mark.
#[test]
fn the_model_row_says_what_the_popup_s_row_said() {
    use crate::tui::models::ModelChoice;
    let mut m = ModelChoice {
        provider: "openai".into(),
        model: "gpt-5".into(),
        context_window: 400_000,
        status: Some("ok".into()),
        needs_key: true,
        has_key: false,
    };
    let s = crate::tui::picker::model_row(&m, "strata/qwen3.8");
    assert_eq!(s.label, "openai/gpt-5");
    assert_eq!(s.completion, "/model openai/gpt-5");
    assert_eq!(s.description, "ctx 400k · ok · no key");
    m.needs_key = false;
    let s = crate::tui::picker::model_row(&m, "openai/gpt-5");
    assert_eq!(s.description, "ctx 400k · ok · active");
}

fn model_row(provider: &str, model: &str) -> crate::tui::models::ModelChoice {
    crate::tui::models::ModelChoice {
        provider: provider.into(),
        model: model.into(),
        context_window: 100_000,
        status: None,
        needs_key: false,
        has_key: true,
    }
}

/// Three models, the second active: an empty filter highlights it;
/// a filter that keeps a row highlights the first match; an active
/// model that is not among the rows falls back to the first row.
#[test]
fn the_model_list_highlights_the_active_model_and_the_first_match() {
    let mut state = view();
    state.set_models(std::sync::Arc::new(vec![
        model_row("strata", "inkling"),
        model_row("deepseek", "flash"),
        model_row("glm", "air"),
    ]));
    state.set_active_model_for_test("deepseek/flash");
    state.editor.set_text("/model");
    state.sync_picker();
    assert_eq!(
        names(&state),
        ["strata/inkling", "deepseek/flash", "glm/air"],
        "the state's models, in its order"
    );
    assert_eq!(highlighted(&state), 1, "the active model's row");

    state.editor.set_text("/model glm");
    state.sync_picker();
    assert_eq!(names(&state), ["glm/air"], "the filter's rows");
    assert_eq!(highlighted(&state), 0, "the first match");

    // A model that is not among the rows: the first row again.
    state.editor.set_text("");
    state.sync_picker();
    state.set_active_model_for_test("gone/model");
    state.editor.set_text("/model");
    state.sync_picker();
    assert_eq!(
        highlighted(&state),
        0,
        "a missing active model: the first row"
    );
}

/// A model row filters on its label and its description too: a status
/// word, the missing-key mark and `active` all find it, as a skill's
/// description does.
#[test]
fn a_model_row_filters_on_its_description_too() {
    let mut loaded = model_row("strata", "inkling");
    loaded.status = Some("loaded".into());
    let mut keyless = model_row("glm", "air");
    keyless.needs_key = true;
    keyless.has_key = false;
    let mut state = view();
    state.set_models(std::sync::Arc::new(vec![
        loaded,
        keyless,
        model_row("deepseek", "flash"),
    ]));
    state.set_active_model_for_test("deepseek/flash");
    state.editor.set_text("/model loaded");
    state.sync_picker();
    assert_eq!(names(&state), ["strata/inkling"], "a status word");
    state.editor.set_text("/model no key");
    state.sync_picker();
    assert_eq!(names(&state), ["glm/air"], "the missing-key mark");
    state.editor.set_text("/model active");
    state.sync_picker();
    assert_eq!(names(&state), ["deepseek/flash"], "the active mark");
}

/// `picker_action()` on a model row is its row's command; `picker_tab`
/// writes it to the composer, and the list stays.
#[test]
fn the_model_row_acts_on_its_command_and_tab_writes_it() {
    let mut state = view();
    state.set_models(std::sync::Arc::new(vec![model_row("deepseek", "flash")]));
    state.editor.set_text("/model");
    state.sync_picker();
    assert_eq!(
        state.picker_action(),
        Some(Pick::Command("/model deepseek/flash".into())),
        "the row's command"
    );
    state.picker_tab();
    assert_eq!(state.editor.text(), "/model deepseek/flash");
    assert!(state.picker_open(), "the list stays on the completion");
    assert_eq!(
        state.picker_matches().len(),
        1,
        "the completion is the filter's own text: it keeps one row"
    );
    assert_eq!(
        state.picker_matches()[0].label().map(str::to_string),
        Some("deepseek/flash".into()),
        "the one row is the completed model"
    );
}

/// Rows that arrive while the list is open (a `set_models` refresh)
/// keep the highlight on the `provider/model` it stood on, and give
/// back the list's default when that model is gone.
#[test]
fn a_refresh_keeps_the_highlight_on_the_same_model() {
    let mut state = view();
    state.set_models(std::sync::Arc::new(vec![
        model_row("a", "m1"),
        model_row("a", "m2"),
    ]));
    state.editor.set_text("/model");
    state.sync_picker();
    state.picker_next();
    assert_eq!(highlighted(&state), 1, "standing on a/m2");

    // A reordered and grown set: a/m2 is still in it.
    state.set_models(std::sync::Arc::new(vec![
        model_row("b", "m9"),
        model_row("a", "m1"),
        model_row("a", "m2"),
    ]));
    assert!(state.picker_open(), "the list stays on the new rows");
    assert_eq!(
        names(&state),
        ["b/m9", "a/m1", "a/m2"],
        "the new rows, in their order"
    );
    assert_eq!(highlighted(&state), 2, "the model it stood on, by label");

    // It is gone: the default again.
    state.set_models(std::sync::Arc::new(vec![model_row("a", "m1")]));
    assert!(state.picker_open(), "still open");
    assert_eq!(highlighted(&state), 0, "the model is gone: the default");
}

/// A failed model switch keeps the composer text and the list, and
/// says the error in the panel's last row.
#[test]
fn a_failed_model_switch_keeps_the_text_and_the_list_and_sets_the_error() {
    let mut state = view();
    let mut no_key = model_row("deepseek", "flash");
    no_key.needs_key = true;
    no_key.has_key = false;
    state.set_models(std::sync::Arc::new(vec![no_key]));
    state.editor.set_text("/model deepseek");
    state.sync_picker();
    assert!(
        matches!(state.picker_action(), Some(Pick::Command(_))),
        "the model row's command"
    );
    state.pick_failed("no key for deepseek: set GG_DEEPSEEK_API_KEY");
    assert!(state.picker_open(), "the list stays");
    assert_eq!(state.editor.text(), "/model deepseek", "the text stays");
    assert_eq!(
        state.picker.as_ref().unwrap().error.as_deref(),
        Some("no key for deepseek: set GG_DEEPSEEK_API_KEY"),
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
