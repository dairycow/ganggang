//! The history file: `~/.gg/history.jsonl`, one JSON string per line,
//! newest last.
//!
//! Best effort by design. History is a convenience: nothing here may make
//! a run fail, so every failure is swallowed by the caller (`History`) and
//! the in-memory list stands on its own. What is *not* swallowed silently
//! is a malformed line: a load counts them and [`super::History::warning`]
//! turns the count into one line of feedback.

use std::fs::{OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// A file this many lines long is rewritten compactly rather than left to
/// grow: the cap keeps a rewrite rare, not small.
pub(super) const COMPACT_AFTER: usize = 1000;

/// `~/.gg/history.jsonl`. `None` without HOME (see [`crate::home`]) — no
/// home, no history file, and the history stays in memory for the run.
pub(super) fn path() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".gg").join("history.jsonl"))
}

/// The newest `keep` prompts, oldest first, plus the count of lines that
/// were not a prompt. Reading also makes sure the file is the user's
/// alone: the prompts are private whether gg wrote them or something else
/// created the file.
pub(super) fn read(path: &Path, keep: usize) -> (Vec<String>, usize) {
    let Ok(file) = OpenOptions::new().read(true).open(path) else {
        return (Vec::new(), 0);
    };
    tighten(path);
    let mut kept = Vec::new();
    let mut skipped = 0usize;
    for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
        let Ok(line) = line else { break };
        match parse_prompt(&line) {
            Some(text) => {
                kept.push(text);
                if kept.len() > keep {
                    kept.remove(0);
                }
            }
            None => skipped += 1,
        }
    }
    (kept, skipped)
}

/// One line as a prompt: a JSON string holding text. Everything else — a
/// torn tail, a number, an object, a string that trims to nothing — is not
/// history. `from_str` rejects trailing characters, so `"a" "b"` is not a
/// line either.
fn parse_prompt(line: &str) -> Option<String> {
    let value = serde_json::from_str::<String>(line).ok()?;
    let text = value.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The file's line count, for the compaction decision. Unreadable counts
/// as zero, which makes the next write an append.
pub(super) fn count_lines(path: &Path) -> usize {
    let Ok(file) = OpenOptions::new().read(true).open(path) else {
        return 0;
    };
    std::io::BufRead::lines(std::io::BufReader::new(file)).count()
}

/// Append one entry. A missing parent directory is created (this is what
/// makes the first run's `~/.gg` appear). True when the line is on disk.
pub(super) fn append(path: &Path, text: &str) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(text) else {
        return false;
    };
    let Ok(mut file) = OpenOptions::new()
        .mode(0o600)
        .create(true)
        .append(true)
        .open(path)
    else {
        return false;
    };
    if writeln!(file, "{line}")
        .and_then(|()| file.flush())
        .is_err()
    {
        return false;
    }
    tighten(path);
    true
}

/// Rewrite the whole file through a sibling temp file and one rename: a
/// crash part-way leaves the old history rather than half of a new one.
/// `create_new` keeps a planted temp name from being followed. True when
/// the new file is in place.
pub(super) fn compact(path: &Path, entries: &[String]) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "history.jsonl".to_string());
    let temp = parent.join(format!("{name}.tmp-{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .mode(0o600)
            .create_new(true)
            .write(true)
            .open(&temp)?;
        for entry in entries {
            writeln!(file, "{}", serde_json::to_string(entry)?)?;
        }
        file.flush()
    })();
    if written.is_err() || std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
        return false;
    }
    tighten(path);
    true
}

/// `0o600`, stated on every path that touches the file: opened `mode` only
/// applies at creation, and a file some other program left world-readable
/// is tightened on the way in — the same discipline gg's draft and
/// restart-plan files follow.
fn tighten(path: &Path) {
    let _ = std::fs::set_permissions(path, Permissions::from_mode(0o600));
}
