//! Tests for [`super`]: the scan's listing as it is stored on the
//! state's cache, and where a chosen session continues.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::tui::history::History;

/// A `/session` rig: a state, the channels the handler writes to, and a
/// factory that records what it is asked for, and the directory, and then
/// refuses (no agent is built in these tests).
struct SessionRig {
    state: super::super::super::view::State,
    input_tx: tokio::sync::mpsc::Sender<super::super::super::AgentInput>,
    ui_tx: tokio::sync::mpsc::Sender<super::super::super::agent_task::UiEvent>,
    factory: super::super::super::Factory,
    /// Every session the factory was asked to open, in order, and the
    /// directory it was asked to run it in: `file <path> in <directory>`.
    asked: Arc<Mutex<Vec<String>>>,
    config: crate::config::Config,
    active_endpoint: crate::config::Endpoint,
    cwd: std::path::PathBuf,
    abort: Arc<AtomicBool>,
}

impl SessionRig {
    fn new() -> Self {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorder = asked.clone();
        let factory: super::super::super::Factory = Arc::new(move |pick, directory, _, _| {
            let asked = match pick {
                super::super::super::SessionPick::Path(path) => {
                    format!("file {} in {}", path.display(), directory.display())
                }
                super::super::super::SessionPick::Prefix(id) => format!("prefix {id}"),
                super::super::super::SessionPick::New => "new".to_string(),
                super::super::super::SessionPick::Latest => "latest".to_string(),
            };
            recorder.lock().unwrap().push(asked);
            anyhow::bail!("no agent is built in this test")
        });
        let (input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::channel(4);
        let config = crate::config::Config::default();
        let active_endpoint = config.active_endpoint();
        Self {
            state: super::super::super::view::State::new(
                std::path::Path::new("/tmp/x"),
                "m",
                131_072,
                Arc::new(vec![]),
                History::default(),
            ),
            input_tx,
            ui_tx,
            factory,
            asked,
            config,
            active_endpoint,
            cwd: std::path::PathBuf::from("/tmp/x"),
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    fn ctx(&mut self) -> CmdCtx<'_> {
        let Self {
            state,
            input_tx,
            ui_tx,
            factory,
            config,
            active_endpoint,
            cwd,
            abort,
            ..
        } = self;
        CmdCtx {
            state,
            factory,
            input_tx,
            ui_tx,
            skills: Arc::new(Vec::new()),
            cwd,
            abort,
            config,
            active_endpoint,
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// The listing as the cache holds it: stored, not scanning, and
    /// where this directory's sessions are filed under the test's HOME.
    fn listing(&self) -> (Vec<String>, bool, bool) {
        let cache = &self.state.sessions;
        let ids: Vec<String> = cache
            .everywhere
            .iter()
            .map(|session| session.path.display().to_string())
            .collect();
        (ids, cache.loaded, cache.scanning)
    }
}

/// A session filed as `<stamp>-<id>.jsonl`, titled.
fn filed(id: &str, title: &str) -> crate::agent::session::io::SessionFile {
    crate::agent::session::io::SessionFile {
        id: format!("uuid-of-{id}"),
        timestamp: String::new(),
        model: "m".into(),
        cwd: "/tmp/x".into(),
        title: Some(title.into()),
        preview: None,
        path: std::path::PathBuf::from(format!("/s/20261125-120000-{id}.jsonl")),
    }
}

/// A session filed under another project's directory, its header naming
/// that directory.
fn filed_elsewhere(id: &str, title: &str) -> crate::agent::session::io::SessionFile {
    let mut session = filed(id, title);
    session.cwd = "/home/u/other".into();
    session.path = std::path::PathBuf::from(format!("/s/-home-u-other/20261125-120000-{id}.jsonl"));
    session
}

/// The listing a finished scan found: stored on the state's cache, the
/// scan marked not in flight. An empty store, and one that holds only
/// the session in use, are the panel's to say on their own row, not the
/// transcript's.
#[test]
fn the_listed_sessions_are_stored_on_the_cache_and_the_session_in_use_is_kept_out() {
    let mut rig = SessionRig::new();
    let in_use = filed("b2c3d4e5", "older");
    rig.state.session_path = Some(in_use.path.clone());

    rig.state.sessions_listed(
        vec![filed("a1b2c3d4", "newest"), in_use.clone()],
        Vec::new(),
    );
    let (ids, loaded, scanning) = rig.listing();
    assert_eq!(
        ids,
        [
            "/s/20261125-120000-a1b2c3d4.jsonl",
            "/s/20261125-120000-b2c3d4e5.jsonl",
        ],
        "the listing the scan found, as read"
    );
    assert!(loaded, "marked loaded");
    assert!(!scanning, "no scan in flight after one is stored");
    assert!(
        !rig.state
            .line_texts_for_test()
            .iter()
            .any(|l| l.contains("no sessions") || l.contains("no other session")),
        "nothing is said: an empty list is a panel row, not a transcript line"
    );

    // A solo session in use, and a store with nothing in it: the cache
    // holds what was read, and the panel is where the row is said.
    let mut solo = SessionRig::new();
    solo.state.session_path = Some(in_use.path.clone());
    solo.state.sessions_listed(vec![in_use.clone()], Vec::new());
    let (ids, loaded, scanning) = solo.listing();
    assert_eq!(ids, ["/s/20261125-120000-b2c3d4e5.jsonl"]);
    assert!(loaded && !scanning);
    assert!(solo.listing().0.len() == 1, "nothing else is remembered");
}

/// The listing a scan found, read off the loop: stored on the cache with
/// the scan's warnings whatever else is going on. A run in flight, or
/// the list open on a filter, holds nothing back: the listing is the
/// list's rows whenever it is drawn, and the text stays the user's.
#[test]
fn the_listed_sessions_are_stored_whatever_the_loop_is_doing() {
    let warnings = vec!["skipping session file: read failed".to_string()];
    let mut rig = SessionRig::new();
    sessions_listed(&mut rig.state, vec![filed("a1b2c3d4", "newest")], warnings);
    let (ids, loaded, scanning) = rig.listing();
    assert_eq!(ids, ["/s/20261125-120000-a1b2c3d4.jsonl"]);
    assert!(loaded && !scanning);
    let lines = rig.state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l == "⚠ skipping session file: read failed"),
        "the scan's warnings reach the transcript: {lines:?}"
    );

    let mut busy = SessionRig::new();
    busy.state.busy = true;
    sessions_listed(
        &mut busy.state,
        vec![filed("a1b2c3d4", "newest")],
        Vec::new(),
    );
    assert_eq!(
        busy.listing().0.len(),
        1,
        "a run in flight does not hold the listing back"
    );

    let mut draft = SessionRig::new();
    draft.state.editor.set_text("/session other");
    sessions_listed(
        &mut draft.state,
        vec![filed("a1b2c3d4", "newest")],
        Vec::new(),
    );
    assert_eq!(draft.listing().0.len(), 1, "nor does a filter");
    assert_eq!(
        draft.state.editor.text(),
        "/session other",
        "the text is the user's: it stays"
    );
    let lines = draft.state.line_texts_for_test();
    assert!(
        !lines.iter().any(|l| l.contains("sessions are listed")),
        "and nothing is said about it: {lines:?}"
    );
}

/// The scan's warnings reach the transcript when they differ from the
/// previous scan's, and not again when they are the same.
#[test]
fn the_scan_says_its_warnings_only_when_they_change() {
    let warnings = vec!["skipping session file: read failed".to_string()];
    let mut rig = SessionRig::new();
    rig.state.sessions_listed(Vec::new(), warnings.clone());
    let first = rig.state.line_texts_for_test();
    assert!(
        first
            .iter()
            .any(|l| l == "⚠ skipping session file: read failed"),
        "{first:?}"
    );

    rig.state.sessions_listed(Vec::new(), warnings.clone());
    let again = rig.state.line_texts_for_test();
    assert_eq!(
        first, again,
        "the same warnings are not repeated: {again:?}"
    );

    rig.state
        .sessions_listed(Vec::new(), vec!["another file: read failed".into()]);
    let changed = rig.state.line_texts_for_test();
    assert!(
        changed.iter().any(|l| l == "⚠ another file: read failed"),
        "a new warning is said: {changed:?}"
    );
}

/// The row the picker highlighted opens through the factory by its file,
/// in the directory the session started in: a session of another project
/// as readily as one of this project's. A failed open leaves the picker
/// as it was — open, with its filter and its highlight — and nothing in
/// the transcript beyond the error the caller reports.
#[test]
fn the_picked_row_opens_through_the_factory_by_its_file_in_its_own_directory() {
    let mut rig = SessionRig::new();
    let own = std::env::temp_dir().join(format!("gg-resume-own-{}", std::process::id()));
    std::fs::create_dir_all(&own).unwrap();
    let mut session = filed_elsewhere("c3d4e5f6", "other repo");
    session.cwd = own.display().to_string();

    // The list open on two sessions, its filter and highlight the
    // user's own: a failure must leave them both where they were.
    let mut pick = super::super::super::picker::tests::pick_for_test(2);
    pick.selected = 1;
    rig.state.picker = Some(pick);
    rig.state.editor.set_text("/session other");

    let error = open_session(session, rig.ctx()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no agent is built in this test",
        "the factory's error is the caller's to report"
    );
    assert!(rig.state.picker_open(), "a failed open loses no picker");
    assert_eq!(
        rig.state.editor.text(),
        "/session other",
        "nor the text that opened it"
    );
    assert_eq!(
        rig.state.picker.as_ref().unwrap().selected,
        1,
        "nor the highlight"
    );
    assert_eq!(
        rig.asked(),
        [format!(
            "file /s/-home-u-other/20261125-120000-c3d4e5f6.jsonl in {}",
            own.display()
        )],
        "the session's own directory is where the agent is asked to run"
    );
    assert_eq!(
        rig.cwd,
        std::path::PathBuf::from("/tmp/x"),
        "a failed open moves nothing"
    );
    assert!(
        !rig.state
            .line_texts_for_test()
            .iter()
            .any(|l| l.contains("✗")),
        "the error is the caller's to report: {:#?}",
        rig.state.line_texts_for_test()
    );
    let _ = std::fs::remove_dir_all(&own);
}

/// The four lines a session's info says: the path, the model, the
/// entries, the token totals.
#[test]
fn session_info_says_the_path_the_model_the_entries_and_the_tokens() {
    let mut rig = SessionRig::new();
    let file = std::env::temp_dir().join(format!("gg-session-info-{}.jsonl", std::process::id()));
    std::fs::write(&file, "{}\n").unwrap();
    rig.state.session_path = Some(file.clone());
    rig.state.entries = 12;
    rig.state.tokens_in = 1_000;
    rig.state.tokens_out = 50;
    session_info(&mut rig.state, "strata/qwen3.8");
    assert_eq!(
        rig.state.line_texts_for_test(),
        [
            format!("session: {}", file.display()),
            "model: strata/qwen3.8".to_string(),
            "entries: 12".to_string(),
            "tokens: 1000 in / 50 out / 0 cache".to_string(),
        ]
    );
    let _ = std::fs::remove_file(&file);
}

/// A fresh session's file is written on its first message, which the
/// path line says while it does not exist; a `--no-session` run has no
/// path at all, and the block says so.
#[test]
fn session_info_says_when_the_file_is_written_and_when_it_is_not_persisted() {
    let mut rig = SessionRig::new();
    let missing =
        std::env::temp_dir().join(format!("gg-session-info-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&missing);
    rig.state.session_path = Some(missing.clone());
    session_info(&mut rig.state, "m");
    let expected = format!(
        "session: {} (not written until the first message)",
        missing.display()
    );
    assert!(
        rig.state
            .line_texts_for_test()
            .iter()
            .any(|line| line.starts_with(&expected)),
        "the missing file is said: {:?}",
        rig.state.line_texts_for_test()
    );
    let _ = std::fs::remove_file(&missing);

    let mut no_session = SessionRig::new();
    no_session.state.sessions_disabled = true;
    session_info(&mut no_session.state, "m");
    assert!(
        no_session
            .state
            .line_texts_for_test()
            .iter()
            .any(|line| line.starts_with("session: not persisted (--no-session)")),
        "no path: not persisted: {:?}",
        no_session.state.line_texts_for_test()
    );
}

/// `copy path` on a session with a file: the write gets the path, and
/// the transcript says where it went.
#[test]
fn copy_session_path_copies_the_session_file_path() {
    let mut rig = SessionRig::new();
    rig.state.session_path = Some(std::path::PathBuf::from("/s/a1b2c3d4.jsonl"));
    let copied = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = copied.clone();
    let result = copy_session_path(&mut rig.state, |text| {
        recorded.lock().unwrap().push(text.to_string());
        Ok(())
    });
    assert!(
        result.is_ok(),
        "a write that succeeds is no error: {result:?}"
    );
    assert_eq!(
        *copied.lock().unwrap(),
        ["/s/a1b2c3d4.jsonl"],
        "the path, as it is on disk"
    );
    assert!(
        rig.state
            .line_texts_for_test()
            .iter()
            .any(|line| line == "copied /s/a1b2c3d4.jsonl"),
        "the transcript says what was copied: {:?}",
        rig.state.line_texts_for_test()
    );
}

/// `copy path` with no session file says the run is `--no-session`, and
/// a copy that fails says so.
#[test]
fn copy_session_path_says_no_session_and_says_when_the_write_fails() {
    let mut no_session = SessionRig::new();
    no_session.state.sessions_disabled = true;
    let error = copy_session_path(&mut no_session.state, |text| {
        panic!("nothing to write: {text:?}")
    })
    .unwrap_err();
    assert_eq!(error, "no session file to copy: this run is --no-session");
    assert!(
        no_session.state.line_texts_for_test().is_empty(),
        "nothing is copied, nothing is said"
    );

    let mut failing = SessionRig::new();
    failing.state.session_path = Some(std::path::PathBuf::from("/s/a1b2c3d4.jsonl"));
    let error = copy_session_path(&mut failing.state, |_text| {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no tty"))
    })
    .unwrap_err();
    assert!(
        error.starts_with("cannot copy"),
        "the failed write is the error: {error:?}"
    );
}

/// A session continues in the directory it started in for as long as that
/// directory exists, wherever gg is working. A directory that is gone
/// leaves it where it is, and says so; a header that names none says
/// nothing.
#[test]
fn a_session_continues_in_its_own_directory_while_it_has_one() {
    let work = std::env::temp_dir().join(format!("gg-resume-here-{}", std::process::id()));
    let here = work.join("here");
    let own = work.join("own");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::create_dir_all(&own).unwrap();
    let mut session = filed("a1b2c3d4", "here");

    session.cwd = here.display().to_string();
    let stays = continuation(&session, &here);
    assert_eq!(
        (stays.directory.as_path(), stays.note.as_deref()),
        (here.as_path(), None),
        "already there: nothing to say"
    );

    session.cwd = own.display().to_string();
    let moves = continuation(&session, &here);
    assert_eq!(moves.directory, own);
    let said = format!("this session runs in {}", own.display());
    assert_eq!(moves.note.as_deref(), Some(said.as_str()));
    let _ = std::fs::remove_dir_all(&work);

    session.cwd = "/nowhere/at/all".into();
    let gone = continuation(&session, &here);
    assert_eq!(gone.directory, here);
    let said = format!(
        "the directory this session started in (/nowhere/at/all) is gone; it continues in {}",
        here.display()
    );
    assert_eq!(gone.note.as_deref(), Some(said.as_str()));

    session.cwd = String::new();
    let nameless = continuation(&session, &here);
    assert_eq!(
        (nameless.directory.as_path(), nameless.note),
        (here.as_path(), None)
    );
}
