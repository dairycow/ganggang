//! Tests for [`super`]: the picker's opening, the listing read off the
//! loop, and where a chosen session continues.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::super::{CmdCtx, Command, handle_command};
use super::*;
use crate::tui::history::History;

/// A `/resume` rig: a state, the channels the handler writes to, and a
/// factory that records what it is asked for, and the directory, and then
/// refuses (no agent is built in these tests).
struct ResumeRig {
    state: super::super::super::view::State,
    input_tx: tokio::sync::mpsc::Sender<super::super::super::AgentInput>,
    ui_tx: tokio::sync::mpsc::Sender<super::super::super::agent_task::UiEvent>,
    factory: super::super::super::Factory,
    /// Every session the factory was asked to open, in order, and the
    /// directory it was asked to run it in: `file <path> in <directory>`.
    asked: Arc<Mutex<Vec<String>>>,
    config: crate::config::Config,
    active_endpoint: crate::config::Endpoint,
    hooks: crate::agent::hooks::Hooks,
    cwd: std::path::PathBuf,
    abort: Arc<AtomicBool>,
}

impl ResumeRig {
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
            hooks: crate::agent::hooks::Hooks::default(),
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
            hooks,
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
            hooks,
            cwd,
            abort,
            config,
            active_endpoint,
        }
    }

    async fn run(&mut self, command: Command) -> anyhow::Result<bool> {
        handle_command(command, self.ctx()).await
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
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

/// A bare `/resume` opens the picker on every project's sessions and
/// puts nothing in the transcript, in the order `/resume <n>` counts. A
/// store with nothing in it is still one line there.
#[test]
fn a_bare_resume_opens_the_picker_on_every_project_and_prints_nothing() {
    let mut rig = ResumeRig::new();
    let here = std::path::Path::new("/s");
    let home = Some(std::path::PathBuf::from("/home/u"));
    open_picker(
        &mut rig.state,
        vec![
            filed_elsewhere("c3d4e5f6", "other repo"),
            filed("a1b2c3d4", "newest"),
            filed("b2c3d4e5", "older"),
        ],
        here,
        home.clone(),
    );
    assert!(rig.state.picker_open());
    assert!(
        rig.state.session_scope_all(),
        "every project's sessions, to begin with"
    );
    let names: Vec<String> = rig
        .state
        .picker_matches()
        .iter()
        .map(|row| row.name.clone())
        .collect();
    assert_eq!(
        names,
        ["other repo", "newest", "older"],
        "one listing, in the order it was read"
    );
    assert_eq!(
        rig.state.pending_resume.len(),
        3,
        "the order `/resume <n>` counts"
    );
    assert!(
        rig.state.line_texts_for_test().is_empty(),
        "the listing is read in the picker, not printed"
    );

    open_picker(&mut rig.state, Vec::new(), here, home);
    assert!(!rig.state.picker_open(), "nothing to pick from: no picker");
    assert!(
        rig.state.pending_resume.is_empty(),
        "and no listing for a number to count"
    );
    let lines = rig.state.line_texts_for_test();
    assert!(lines.iter().any(|l| l == "no sessions yet"), "{lines:?}");
}

/// The session gg is in now is not a row: reopening it would only
/// reload it from disk, and it is the row Enter would open by default.
/// `/resume <n>` counts the rest, and a store that holds only the
/// session in use gets one line in the transcript and no picker.
#[test]
fn the_session_in_use_is_not_listed_by_the_picker() {
    let mut rig = ResumeRig::new();
    let here = std::path::Path::new("/s");
    let in_use = filed("b2c3d4e5", "older");
    rig.state.session_path = Some(in_use.path.clone());

    open_picker(
        &mut rig.state,
        vec![filed("a1b2c3d4", "newest"), in_use.clone()],
        here,
        None,
    );
    assert!(rig.state.picker_open());
    let ids: Vec<&str> = rig
        .state
        .picker_matches()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(ids, ["a1b2c3d4"], "the session in use is not a row");
    assert_eq!(
        rig.state.pending_resume.len(),
        1,
        "and /resume <n> counts the rest"
    );

    let mut solo = ResumeRig::new();
    solo.state.session_path = Some(in_use.path.clone());
    open_picker(&mut solo.state, vec![in_use], here, None);
    assert!(
        !solo.state.picker_open(),
        "nothing else to pick from: no picker"
    );
    assert!(
        solo.state.pending_resume.is_empty(),
        "and no listing for a number to count"
    );
    let lines = solo.state.line_texts_for_test();
    assert!(
        lines.iter().any(|l| l == "no other session yet"),
        "{lines:?}"
    );
}

/// The listing a bare `/resume` asked for, read off the loop: idle with
/// an empty composer, it opens the picker and pushes the scan's
/// warnings; a run in flight, or a draft typed while the store was
/// read, keeps it unopened and says so.
#[test]
fn the_listed_sessions_open_the_picker_when_idle_and_say_so_when_not() {
    let cwd = std::path::Path::new("/tmp/x");
    // The picker's rows are built against this directory's sessions dir,
    // which is under HOME: a test's home, not the machine's.
    let work = std::env::temp_dir().join(format!("gg-resume-listed-{}", std::process::id()));
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let saved_home = std::env::var_os("HOME");
    unsafe { std::env::set_var("HOME", &home) };

    let warnings = vec!["skipping session file: read failed".to_string()];
    let mut rig = ResumeRig::new();
    sessions_listed(
        &mut rig.state,
        vec![filed("a1b2c3d4", "newest")],
        warnings,
        cwd,
    );
    assert!(
        rig.state.picker_open(),
        "idle and empty composer: the picker opens"
    );
    let lines = rig.state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l == "⚠ skipping session file: read failed"),
        "the scan's warnings reach the transcript: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l == "sessions are listed: /resume again to pick one"),
        "nothing held the listing back: {lines:?}"
    );

    let mut busy = ResumeRig::new();
    busy.state.busy = true;
    sessions_listed(
        &mut busy.state,
        vec![filed("a1b2c3d4", "newest")],
        Vec::new(),
        cwd,
    );
    assert!(
        !busy.state.picker_open(),
        "a run in flight keeps it unopened"
    );
    let lines = busy.state.line_texts_for_test();
    assert!(
        lines
            .iter()
            .any(|l| l == "sessions are listed: /resume again to pick one"),
        "and the transcript says so: {lines:?}"
    );

    let mut draft = ResumeRig::new();
    draft.state.editor.set_text("a draft, not a filter");
    sessions_listed(
        &mut draft.state,
        vec![filed("a1b2c3d4", "newest")],
        Vec::new(),
        cwd,
    );
    assert!(
        !draft.state.picker_open(),
        "a draft typed while the store was read keeps it unopened"
    );
    assert_eq!(
        draft.state.editor.text(),
        "a draft, not a filter",
        "the draft is the user's: it stays"
    );

    unsafe {
        match saved_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The row the picker highlighted opens through the factory by its file,
/// in the directory the session started in: a session of another project
/// as readily as one of this project's, with none of the prefix
/// resolution. A failed open leaves the picker as it was — open, with
/// its filter and its highlight — and nothing in the transcript beyond
/// the error the caller reports.
#[test]
fn the_picked_row_opens_through_the_factory_by_its_file_in_its_own_directory() {
    let mut rig = ResumeRig::new();
    let own = std::env::temp_dir().join(format!("gg-resume-own-{}", std::process::id()));
    std::fs::create_dir_all(&own).unwrap();
    let mut session = filed_elsewhere("c3d4e5f6", "other repo");
    session.cwd = own.display().to_string();

    // The picker open on two sessions, its filter and highlight the
    // user's own: a failure must leave them both where they were.
    let mut pick = super::super::super::picker::Picker::new(
        vec![filed("a1b2c3d4", "newest"), filed("b2c3d4e5", "older")],
        vec![filed("a1b2c3d4", "newest"), filed("b2c3d4e5", "older")],
        None,
    );
    pick.selected = 1;
    pick.filter = "other".into();
    rig.state.picker = Some(pick);
    rig.state.editor.set_text("other");

    let error = open_session(session, rig.ctx()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no agent is built in this test",
        "the factory's error is the caller's to report"
    );
    assert!(rig.state.picker_open(), "a failed open loses no picker");
    assert_eq!(
        rig.state.editor.text(),
        "other",
        "nor the filter the picker was built on"
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

/// `/resume <n>` counts the listing the picker opened, newest first, and
/// past its end is the error it always was.
#[tokio::test]
async fn a_number_counts_the_listing_the_picker_opened() {
    let mut rig = ResumeRig::new();
    let mut older = filed("b2c3d4e5", "older");
    older.cwd = "/gg-tests/no-such-directory".into();
    open_picker(
        &mut rig.state,
        vec![filed("a1b2c3d4", "newest"), older],
        std::path::Path::new("/s"),
        None,
    );
    assert!(rig.run(Command::Resume(Some("2".into()))).await.is_err());
    assert_eq!(
        rig.asked(),
        ["file /s/20261125-120000-b2c3d4e5.jsonl in /tmp/x"]
    );
    assert_eq!(
        rig.run(Command::Resume(Some("3".into())))
            .await
            .unwrap_err()
            .to_string(),
        "3 is not in the last /resume listing"
    );
    assert_eq!(rig.asked().len(), 1, "nothing was asked for");
}

/// `--no-session` refuses `/resume` in both shapes: a warning, no
/// picker, and nothing asked of the factory.
#[tokio::test]
async fn no_session_refuses_the_picker_and_the_argument() {
    let mut rig = ResumeRig::new();
    rig.state.sessions_disabled = true;
    assert!(!rig.run(Command::Resume(None)).await.unwrap());
    assert!(!rig.run(Command::Resume(Some("a1b2".into()))).await.unwrap());
    assert!(!rig.state.picker_open());
    assert!(rig.asked().is_empty());
    let lines = rig.state.line_texts_for_test();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("/resume is disabled for --no-session runs"))
            .count(),
        2,
        "{lines:?}"
    );
}

/// `/resume <prefix>` names a session of this project first. When none
/// matches, it names one of any project's, so an id the all-projects view
/// showed can be typed from here.
#[test]
fn a_prefix_falls_back_to_every_project_when_none_here_matches() {
    let root = std::env::temp_dir().join(format!("gg-resume-prefix-{}", std::process::id()));
    let header = |id: &str, cwd: &str| {
        format!(
            "{{\"type\":\"session\",\"version\":1,\"id\":\"{id}\",\"timestamp\":\"t\",\"cwd\":\"{cwd}\",\"model\":\"m\"}}\n"
        )
    };
    let projects: &[(&str, &[(&str, &str)])] = &[
        (
            "-proj-here",
            &[
                ("aaaa1111-0000", "/proj/here"),
                ("aacc3333-0000", "/proj/here"),
            ],
        ),
        ("-proj-there", &[("bbbb2222-0000", "/proj/there")]),
    ];
    for &(slug, sessions) in projects {
        let dir = root.join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        for (id, cwd) in sessions {
            std::fs::write(
                dir.join(format!("20260101-0900-{}.jsonl", &id[..8])),
                header(id, cwd),
            )
            .unwrap();
        }
    }
    let here = std::path::Path::new("/proj/here");

    let elsewhere = resolve_prefix_everywhere(&root, here, "bbbb").unwrap();
    assert_eq!(
        elsewhere.id, "bbbb2222-0000",
        "nothing here matches: every project is searched"
    );
    assert_eq!(elsewhere.cwd, "/proj/there");

    // This project's matches come first, its own ambiguity included: that
    // is not settled by looking elsewhere.
    let ambiguous = resolve_prefix_everywhere(&root, here, "aa").unwrap_err();
    assert!(
        format!("{ambiguous:#}").contains("ambiguous"),
        "{ambiguous:#}"
    );
    assert_eq!(
        resolve_prefix_everywhere(&root, here, "aacc").unwrap().id,
        "aacc3333-0000"
    );
    assert_eq!(
        resolve_prefix_everywhere(&root, here, "aaaa1111")
            .unwrap()
            .id,
        "aaaa1111-0000",
        "the 8-hex id in the file name"
    );

    let none = resolve_prefix_everywhere(&root, here, "zz").unwrap_err();
    assert!(none.to_string().contains("no session matches"), "{none:#}");
    let _ = std::fs::remove_dir_all(&root);
}
