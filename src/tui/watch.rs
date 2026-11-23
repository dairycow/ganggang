//! `--watch`: rebuild and re-exec on source changes (the development loop).
//!
//! Nothing here draws; the TUI owns the screen. A background task polls a
//! snapshot of the project's sources, runs `cargo build` once an edit has
//! settled, and reports on a channel. The event loop decides when a restart
//! is safe and returns [`Exit::Restart`](super::Exit) so `main` re-execs
//! after the terminal is the shell's again.
//!
//! Only the ganggang workspace is watched: the metadata check refuses any
//! project that does not build a `gg` binary, so running `gg --watch` in
//! another project can never swap in that project's executable.

pub mod config;
pub mod restart;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio::time::sleep;

pub use config::Config;
pub use restart::{Restart, restart_plan, take_draft};

/// How often the source snapshot is compared.
const POLL: Duration = Duration::from_millis(400);
/// How long an edit must sit still before one build is started.
const QUIET: Duration = Duration::from_millis(600);
/// Cargo's output on failure, in lines and in characters, so a wall of
/// diagnostics cannot swallow the transcript.
const FAILURE_TAIL_LINES: usize = 20;
const FAILURE_TAIL_CHARS: usize = 4_000;

/// A file's identity for change detection: mtime and length. Same-size
/// edits within one mtime tick would slip through, which no editor does.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot(BTreeMap<PathBuf, (SystemTime, u64)>);

impl Snapshot {
    fn scan(roots: &[PathBuf]) -> Self {
        let mut files = BTreeMap::new();
        for root in roots {
            let Ok(meta) = std::fs::metadata(root) else {
                continue;
            };
            if meta.is_dir() {
                walk(root, &mut files);
            } else if meta.is_file() {
                record(root, &meta, &mut files);
            }
        }
        Self(files)
    }
}

fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, (SystemTime, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Source files only: editor swap/autosave files (`.*.swp`, `#x#`)
        // sit in the same directory and would otherwise rebuild (and
        // restart) on every few seconds of idle typing in another editor.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk(&path, files);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            record(&path, &meta, files);
        }
    }
}

fn record(path: &Path, meta: &std::fs::Metadata, files: &mut BTreeMap<PathBuf, (SystemTime, u64)>) {
    let stamp = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    files.insert(path.to_path_buf(), (stamp, meta.len()));
}

/// What the background task tells the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Building,
    /// A build succeeded. `manual` marks a Ctrl+R request, which restarts
    /// even with an unsent draft (the draft is carried across).
    Built {
        manual: bool,
    },
    /// A build failed; `tail` is the end of cargo's output for the
    /// transcript. Nothing restarts.
    Failed {
        tail: String,
        manual: bool,
    },
}

/// The loop's handle to the build task. One rebuild at a time: a second
/// request while a build runs is refused rather than queued.
#[derive(Debug, Clone)]
pub struct Handle {
    commands: mpsc::Sender<()>,
}

impl Handle {
    pub fn rebuild(&self) -> bool {
        self.commands.try_send(()).is_ok()
    }
}

/// Start watching `cfg`. The task ends by itself once the receiver drops.
pub fn spawn(cfg: Config) -> (Handle, mpsc::Receiver<Event>) {
    let (events_tx, events_rx) = mpsc::channel(16);
    let (commands_tx, commands_rx) = mpsc::channel(1);
    tokio::spawn(task(cfg, commands_rx, events_tx));
    (
        Handle {
            commands: commands_tx,
        },
        events_rx,
    )
}

async fn task(cfg: Config, mut commands: mpsc::Receiver<()>, events: mpsc::Sender<Event>) {
    let mut seen = Snapshot::scan(&cfg.roots);
    let mut changed_at: Option<Instant> = None;
    // A Ctrl+R request outlives a build whose inputs changed underneath it:
    // the eventual restart is still the user's, and carries the draft.
    let mut manual = false;
    loop {
        tokio::select! {
            _ = sleep(POLL) => {
                let now = Snapshot::scan(&cfg.roots);
                if now != seen {
                    seen = now;
                    changed_at = Some(Instant::now());
                } else if changed_at.is_some_and(|at| at.elapsed() >= QUIET) {
                    changed_at = None;
                    match build(&cfg, manual, &seen, &events).await {
                        Outcome::Closed => return,
                        Outcome::Settled => manual = false,
                        Outcome::Stale => changed_at = Some(Instant::now()),
                    }
                }
            }
            command = commands.recv() => {
                if command.is_none() {
                    return;
                }
                manual = true;
                changed_at = None;
                seen = Snapshot::scan(&cfg.roots);
                match build(&cfg, true, &seen, &events).await {
                    Outcome::Closed => return,
                    Outcome::Settled => manual = false,
                    Outcome::Stale => changed_at = Some(Instant::now()),
                }
            }
        }
    }
}

/// What one build attempt produced, or why it did not count.
enum Outcome {
    /// Reported (succeeded or failed); the tree did not move during it.
    Settled,
    /// The sources changed while cargo ran. Nothing is reported: the edit
    /// is not in that binary, and the debounce builds again. Restarting
    /// now would run a binary that predates the save, and the fresh
    /// process's first snapshot would never notice it.
    Stale,
    /// The event loop is gone.
    Closed,
}

/// Run one build and report it against `before`, the snapshot its inputs
/// had when it started.
async fn build(
    cfg: &Config,
    manual: bool,
    before: &Snapshot,
    events: &mpsc::Sender<Event>,
) -> Outcome {
    if events.send(Event::Building).await.is_err() {
        return Outcome::Closed;
    }
    let mut command = tokio::process::Command::new("cargo");
    command.arg("build");
    if let Some(flag) = cfg.profile.cargo_flag() {
        command.arg(flag);
    }
    command
        .current_dir(&cfg.root)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Quitting gg mid-build must not leave cargo holding the package
        // lock for the user's next build.
        .kill_on_drop(true);
    let output = command.output().await;
    if &Snapshot::scan(&cfg.roots) != before {
        return Outcome::Stale;
    }
    let event = match output {
        Ok(output) if output.status.success() => Event::Built { manual },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let source = if stderr.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout).into_owned()
            } else {
                stderr.into_owned()
            };
            Event::Failed {
                tail: tail(&source, FAILURE_TAIL_LINES),
                manual,
            }
        }
        Err(e) => Event::Failed {
            tail: format!("cannot run cargo: {e}"),
            manual,
        },
    };
    if events.send(event).await.is_ok() {
        Outcome::Settled
    } else {
        Outcome::Closed
    }
}

/// A restart is safe only between runs, with no editor borrowing the tty,
/// and with nothing typed that a restart would drop — unless the user
/// asked for it (Ctrl+R), in which case the draft is carried across.
pub fn can_restart(busy: bool, editor_open: bool, draft_empty: bool, manual: bool) -> bool {
    !busy && !editor_open && (manual || draft_empty)
}

/// The last `lines` lines, capped at [`FAILURE_TAIL_CHARS`] characters.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    let mut kept = all[start..].join("\n");
    let chars = kept.chars().count();
    if chars > FAILURE_TAIL_CHARS {
        kept = kept.chars().skip(chars - FAILURE_TAIL_CHARS).collect();
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn snapshot_notices_adds_changes_and_removals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        write(&root.join("a.rs"), "one");
        let roots = vec![root.clone()];
        let before = Snapshot::scan(&roots);
        assert_eq!(before, Snapshot::scan(&roots), "unchanged scan is stable");

        write(&root.join("nested/b.rs"), "two");
        let added = Snapshot::scan(&roots);
        assert_ne!(before, added);

        write(&root.join("a.rs"), "one, edited");
        let edited = Snapshot::scan(&roots);
        assert_ne!(added, edited);

        std::fs::remove_file(root.join("nested/b.rs")).unwrap();
        let removed = Snapshot::scan(&roots);
        assert_ne!(edited, removed);
    }

    #[test]
    fn snapshot_ignores_non_rust_files_swap_files_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("main.rs"), "fn main() {}");
        let roots = vec![src.clone()];
        let before = Snapshot::scan(&roots);

        // Editor autosave/swap files and non-Rust files are not build input.
        write(&src.join(".main.rs.swp"), "junk");
        write(&src.join("#main.rs#"), "junk");
        write(&src.join("notes.txt"), "junk");
        std::os::unix::fs::symlink(dir.path(), src.join("escape")).unwrap();
        assert_eq!(before, Snapshot::scan(&roots));

        write(&src.join("lib.rs"), "");
        assert_ne!(before, Snapshot::scan(&roots));
    }

    #[test]
    fn snapshot_reads_plain_files_and_tolerates_missing_roots() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        write(&manifest, "[package]");
        let roots = vec![manifest, dir.path().join("gone")];
        assert_eq!(Snapshot::scan(&roots).0.len(), 1);
    }

    #[test]
    fn restart_is_gated_on_idle_and_never_drops_an_automatic_draft() {
        assert!(can_restart(false, false, true, false));
        assert!(!can_restart(true, false, true, false), "run in flight");
        assert!(!can_restart(false, true, true, false), "editor open");
        assert!(
            !can_restart(false, false, false, false),
            "draft would be lost"
        );
        // Ctrl+R carries the draft instead of refusing.
        assert!(can_restart(false, false, false, true));
        assert!(!can_restart(true, false, false, true));
    }

    #[test]
    fn failure_tail_keeps_the_end_and_caps_its_size() {
        let text = "warning: one\nwarning: two\nwarning: three\n";
        assert_eq!(tail(text, 2), "warning: two\nwarning: three");

        let long = "x".repeat(FAILURE_TAIL_CHARS + 10);
        assert_eq!(tail(&long, 1).chars().count(), FAILURE_TAIL_CHARS);
    }
}
