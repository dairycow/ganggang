//! The external editor (Ctrl+G): the composer's text goes to a draft file,
//! `$VISUAL`/`$EDITOR` edits it with the whole terminal, and what was saved
//! replaces the composer. Nothing is sent by itself — the draft comes back
//! for Enter like anything typed. [`command`] picks the program and reads
//! its command line; [`draft`] is the file it edits.

mod command;
mod draft;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::sync::{mpsc, oneshot};

use command::{configured, split_command};

/// How fast an editor can return and still have been used by a person.
/// Faster than this with nothing saved, it opened a window of its own and
/// left gg to carry on without it.
const INSTANT: Duration = Duration::from_millis(500);

/// How an editor session ended, for the loop to apply once the terminal
/// is back.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The draft as saved, as composer text.
    Saved(String),
    /// The composer keeps its text, and why — a warning.
    Unchanged(String),
    /// Nothing to apply, but a line worth reading.
    Notice(String),
}

/// One editor session: the draft on disk and the command that edits it.
#[derive(Debug)]
pub(super) struct Session {
    path: PathBuf,
    program: String,
    args: Vec<String>,
    /// What the draft held when the editor started — whether a failed
    /// session leaves anything worth keeping.
    original: String,
}

/// The loop's hold on a running session: what it needs to end one when it
/// is leaving itself.
#[derive(Debug)]
pub(super) struct Running {
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// The draft and what it held at the start: what is left for the user
    /// when the session is ended rather than finished.
    path: PathBuf,
    original: String,
}

impl Session {
    /// Resolve the editor and write the draft: everything that can fail
    /// before the terminal is given away.
    pub(super) fn prepare(text: &str) -> anyhow::Result<Self> {
        Self::with_command(&configured()?, text)
    }

    fn with_command(line: &str, text: &str) -> anyhow::Result<Self> {
        let dir = draft::dir().with_context(|| {
            format!(
                "opening the draft directory {}",
                draft::dir_path().display()
            )
        })?;
        Self::in_dir(&dir, line, text)
    }

    /// [`Self::with_command`] with the draft in `dir`: the run's own
    /// directory, or a throwaway one in tests.
    fn in_dir(dir: &Path, line: &str, text: &str) -> anyhow::Result<Self> {
        let words = split_command(line);
        let (program, args) = words
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("cannot parse the editor command {line:?}"))?;
        Ok(Self {
            path: draft::write(dir, text)?,
            program: program.clone(),
            args: args.to_vec(),
            original: text.to_string(),
        })
    }

    /// Run the editor and report how it ended. The caller has already
    /// given the terminal away; stdio is inherited, so the editor gets
    /// the real screen and keyboard, and it runs for as long as it likes.
    pub(super) fn launch(self, done: mpsc::Sender<Outcome>) -> Running {
        let (stop, stopped) = oneshot::channel();
        let (path, original) = (self.path.clone(), self.original.clone());
        let task = tokio::spawn(async move {
            // A panic inside the session surfaces as an outcome instead
            // of leaving the screen given away for good.
            let outcome = match tokio::spawn(self.run(stopped)).await {
                Ok(outcome) => outcome,
                Err(e) => Outcome::Unchanged(format!("task panicked: {e}")),
            };
            let _ = done.send(outcome).await;
        });
        Running {
            stop: Some(stop),
            task: Some(task),
            path,
            original,
        }
    }

    async fn run(self, mut stopped: oneshot::Receiver<()>) -> Outcome {
        let started = Instant::now();
        // No process group of its own: the editor has to stay in the
        // terminal's foreground group to read the keyboard.
        let mut command = tokio::process::Command::new(&self.program);
        command.args(&self.args).arg(&self.path).kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_file(&self.path);
                return Outcome::Unchanged(format!(
                    "could not start {program:?}: {e}; composer unchanged",
                    program = self.program
                ));
            }
        };
        let status = tokio::select! {
            status = child.wait() => status,
            Ok(()) = &mut stopped => {
                // gg is exiting. Whatever the editor saved so far stays on
                // disk for the user.
                let _ = child.kill().await;
                return Outcome::Unchanged("was closed because gg is exiting".into());
            }
        };
        outcome(
            status,
            std::fs::read_to_string(&self.path),
            &self,
            started.elapsed() < INSTANT,
        )
    }
}

impl Running {
    /// End the session and wait until the editor is gone. The loop is
    /// leaving with the editor still open: the terminal must not be
    /// restored under a live editor, and the editor must not outlive gg.
    pub(super) async fn abort(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

/// The loop is leaving, an editor possibly still open (the agent task is
/// gone): the session ends first, so the terminal is not restored under a
/// live editor, and the editor does not outlive gg. The tty and the
/// terminal's modes go back for gg's own exit, and the draft directory
/// goes with gg unless a draft is kept in it. What comes back is for the
/// user, once the screen is theirs again.
pub(super) async fn conclude(mut running: Option<Running>) -> anyhow::Result<Option<String>> {
    let mut notes = Vec::new();
    if let Some(running) = running.as_mut() {
        running.abort().await;
        // The editor was ended mid-session. A draft it never touched
        // goes; anything else in it — typed text, or bytes gg cannot read
        // — is the user's to find.
        match std::fs::read_to_string(&running.path) {
            Ok(raw) if draft::composer_text(&raw) == running.original => {
                let _ = std::fs::remove_file(&running.path);
            }
            _ => notes.push(format!(
                "the editor was closed because gg is exiting; what it held is kept at {}",
                running.path.display()
            )),
        }
        notes.extend(super::term::settle());
    }
    draft::cleanup();
    Ok((!notes.is_empty()).then(|| notes.join("\n")))
}

/// What the editor's exit means for the composer. Only a clean exit
/// applies the draft; whatever else happened, the composer keeps its text,
/// and a draft that holds typing of the user's is never deleted.
fn outcome(
    status: io::Result<ExitStatus>,
    saved: io::Result<String>,
    session: &Session,
    instant: bool,
) -> Outcome {
    let path = session.path.display();
    match status {
        Ok(status) if status.success() => match saved {
            Ok(raw) => {
                let _ = std::fs::remove_file(&session.path);
                let text = draft::composer_text(&raw);
                if instant && text == session.original {
                    Outcome::Notice(
                        "— the editor returned at once and left the draft as it was. An \
                         editor that opens its own window comes back before you finish in \
                         it: give it a wait flag (`code --wait`)"
                            .into(),
                    )
                } else {
                    Outcome::Saved(text)
                }
            }
            // Gone, so nothing of the user's is at stake.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Outcome::Unchanged(
                "left no draft to apply (it was deleted); composer unchanged".into(),
            ),
            Err(e) => Outcome::Unchanged(format!(
                "left a draft that cannot be read as text ({e}); composer unchanged; \
                 the draft is kept at {path}"
            )),
        },
        status => {
            let why = match status {
                Ok(status) => status
                    .code()
                    .map(|code| format!("exited with status {code}"))
                    .unwrap_or_else(|| "was killed by a signal".into()),
                Err(e) => format!("could not be waited on ({e})"),
            };
            match saved {
                Ok(raw) => {
                    if draft::composer_text(&raw) == session.original {
                        // Nothing was typed in the editor: a plain cancel.
                        let _ = std::fs::remove_file(&session.path);
                        Outcome::Unchanged(format!("{why}; composer unchanged"))
                    } else {
                        Outcome::Unchanged(format!(
                            "{why}; composer unchanged; the edited draft is kept at {path}"
                        ))
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    Outcome::Unchanged(format!("{why}; composer unchanged"))
                }
                // Not text — but then not what the composer held either,
                // so it is the user's typing in some other encoding.
                Err(e) => Outcome::Unchanged(format!(
                    "{why}; composer unchanged; the draft is kept at {path}, \
                     not readable as text ({e})"
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `command` as the editor over a draft holding "hello", in a
    /// directory of its own: the outcome, and the draft's path.
    async fn run(command: &str) -> (Outcome, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (done, mut done_rx) = mpsc::channel(1);
        let session = Session::in_dir(dir.path(), command, "hello").unwrap();
        let path = session.path.clone();
        session.launch(done);
        (done_rx.recv().await.unwrap(), path, dir)
    }

    #[tokio::test]
    async fn a_saved_draft_comes_back_as_the_next_composer_text() {
        let (outcome, path, _dir) = run(r#"sh -c 'printf " world\r\n" >> "$1"' sh"#).await;
        assert_eq!(outcome, Outcome::Saved("hello world".into()));
        assert!(!path.exists(), "an applied draft is removed");
    }

    #[tokio::test]
    async fn an_editor_that_returns_at_once_leaves_a_hint_and_no_draft() {
        let (outcome, path, _dir) = run("true").await;
        assert!(
            matches!(&outcome, Outcome::Notice(text) if text.contains("wait flag")),
            "{outcome:?}"
        );
        assert!(!path.exists());
    }

    /// The hint is about speed, not about an unchanged draft: an editor
    /// that was used and simply left the text alone is a plain no-op.
    #[test]
    fn the_hint_is_only_for_an_instant_return() {
        let status = || std::process::Command::new("true").status();
        let saved = || Ok("hello\n".to_string());
        let session = Session {
            path: PathBuf::from("/nowhere/draft.md"),
            program: "true".into(),
            args: Vec::new(),
            original: "hello".into(),
        };
        assert!(matches!(
            outcome(status(), saved(), &session, true),
            Outcome::Notice(_)
        ));
        assert_eq!(
            outcome(status(), saved(), &session, false),
            Outcome::Saved("hello".into())
        );
    }

    #[tokio::test]
    async fn a_failed_editor_keeps_the_composer_and_the_edited_draft() {
        let (outcome, path, _dir) = run(r#"sh -c 'echo typed first >> "$1"; exit 3' sh"#).await;
        let Outcome::Unchanged(why) = outcome else {
            panic!("a failing editor must not apply: {outcome:?}")
        };
        assert!(why.contains("exited with status 3"), "{why}");
        assert!(why.contains("composer unchanged"), "{why}");
        assert!(
            why.contains(&path.display().to_string()),
            "names the kept draft: {why}"
        );
        assert!(path.exists(), "the typed text survives the failed editor");
    }

    /// A draft with typing in it that gg cannot read is the user's work in
    /// another encoding: kept, and said to be kept, however the editor
    /// exited.
    #[tokio::test]
    async fn an_unreadable_edited_draft_is_never_deleted() {
        for (editor, exit) in [
            (
                r#"sh -c 'printf "typed\377\n" > "$1"; exit 1' sh"#,
                "exited with status 1",
            ),
            (r#"sh -c 'printf "typed\377\n" > "$1"' sh"#, ""),
        ] {
            let (outcome, path, _dir) = run(editor).await;
            let Outcome::Unchanged(why) = outcome else {
                panic!("{outcome:?}")
            };
            assert!(why.contains("kept at"), "{why}");
            assert!(why.contains(&path.display().to_string()), "{why}");
            if !exit.is_empty() {
                assert!(why.contains(exit), "{why}");
            }
            assert!(path.exists(), "still on disk: {why}");
        }
    }

    #[tokio::test]
    async fn a_cancelled_editor_leaves_no_draft_behind() {
        for (editor, why) in [
            ("sh -c 'exit 1' sh", "exited with status 1"),
            ("sh -c 'kill -9 $$' sh", "was killed by a signal"),
        ] {
            let (outcome, path, _dir) = run(editor).await;
            let Outcome::Unchanged(text) = outcome else {
                panic!("{outcome:?}")
            };
            assert!(text.contains(why), "{text}");
            assert!(text.contains("composer unchanged"), "{text}");
            assert!(!path.exists(), "no edits, nothing to keep");
        }
    }

    #[tokio::test]
    async fn an_editor_that_cannot_start_says_which() {
        let (outcome, path, _dir) = run("gg-no-such-editor").await;
        let Outcome::Unchanged(why) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(
            why.contains(r#"could not start "gg-no-such-editor""#),
            "{why}"
        );
        assert!(why.contains("composer unchanged"), "{why}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_draft_deleted_in_the_editor_is_not_applied_or_called_kept() {
        let (outcome, _, _dir) = run(r#"sh -c 'rm "$1"' sh"#).await;
        let Outcome::Unchanged(why) = outcome else {
            panic!("{outcome:?}")
        };
        assert!(why.contains("composer unchanged"), "{why}");
        assert!(!why.contains("kept"), "nothing is kept: {why}");
    }

    /// The loop is leaving while the editor is open: the session ends, the
    /// editor with it, and nothing hangs on the way out.
    #[tokio::test]
    async fn aborting_a_session_ends_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let (done, mut done_rx) = mpsc::channel(1);
        let session = Session::in_dir(dir.path(), "sh -c 'sleep 30' sh", "").unwrap();
        let running = session.launch(done);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut running = running;
        tokio::time::timeout(Duration::from_secs(5), running.abort())
            .await
            .expect("abort must not wait for the editor");
        assert!(done_rx.recv().await.is_some(), "the session reported");
    }

    /// The loop is leaving with the editor open: a draft the editor never
    /// touched goes without a word; anything else in it — typed text, or
    /// bytes gg cannot read — stays and is named.
    #[tokio::test]
    async fn concluding_a_session_keeps_what_was_typed_and_names_it() {
        for (editor, kept) in [
            ("sh -c 'sleep 30' sh", false),
            (r#"sh -c 'printf typed > "$1"; sleep 30' sh"#, true),
            (r#"sh -c 'printf "typed\377" > "$1"; sleep 30' sh"#, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (done, _done_rx) = mpsc::channel(1);
            let session = Session::in_dir(dir.path(), editor, "hello").unwrap();
            let running = session.launch(done);
            tokio::time::sleep(Duration::from_millis(400)).await;
            let note = tokio::time::timeout(Duration::from_secs(5), conclude(Some(running)))
                .await
                .expect("concluding must not wait for the editor")
                .unwrap();
            let left: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            if kept {
                assert_eq!(left.len(), 1, "{editor}: the draft stays");
                assert!(
                    note.unwrap().contains(&left[0].display().to_string()),
                    "the note names it"
                );
            } else {
                assert!(left.is_empty(), "{editor}: an untouched draft goes");
                assert!(note.is_none(), "{editor}: nothing to say");
            }
        }
    }
}
