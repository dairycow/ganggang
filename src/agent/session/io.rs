//! Session file I/O (spec §13): append-only JSONL under
//! `~/.gg/sessions/<path-slug>/`, one line per entry, flushed per append.
//! Crash safety comes from the append-only shape: a torn trailing line is
//! skipped with a warning on load, never repaired in place. The raw file
//! is the source of truth — replay is `load` + [`super::to_wire`].
//!
//! Listing and resume discovery live in [`list`]; resume-time repairs in
//! [`repair`]; both are re-exported so callers need zero changes.

pub mod list;
pub mod repair;

use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::Entry;
use list::{file_stamp, slug};
use repair::repair_missing_trailing_newline;

/// Re-export the listing and repair APIs so callers need zero changes.
#[allow(unused_imports)] // `list_all_sessions` is exported for future callers
pub use list::{
    SessionFile, latest_in, list_all_sessions, list_sessions, resolve_prefix, sessions_dir,
    sessions_root,
};
pub use repair::repair_dangling_tool_calls;

/// The first line of a session file; `kind` is validated on load so a stray
/// entry-line file cannot pose as a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    kind: String,
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    pub model: String,
}

impl SessionHeader {
    fn new(cwd: &Path, model: &str) -> Self {
        Self {
            kind: "session".to_string(),
            version: 1,
            id: uuid::Uuid::new_v4().hyphenated().to_string(),
            timestamp: super::now_iso(),
            cwd: cwd.display().to_string(),
            model: model.to_string(),
        }
    }
}

/// A session file opened for appending. Dropping it closes the file; every
/// [`append`](Self::append) flushes, so a crash loses at most the entry
/// being written.
///
/// A NEW store is lazy: the file (and its directory) materializes on the
/// first append, so launch-and-quit does not litter header-only files that
/// would outrank real sessions in `-c`'s newest-first listing.
pub struct SessionStore {
    path: PathBuf,
    id: String,
    header_line: String,
    file: Option<std::fs::File>,
    allow_empty_existing: bool,
}

impl SessionStore {
    /// Create a fresh session file for `cwd` under the real `~/.gg/sessions`.
    pub fn create(cwd: &Path, model: &str) -> anyhow::Result<Self> {
        let root = sessions_root().context("storing sessions needs HOME (~/.gg)")?;
        Self::create_under(&root, cwd, model)
    }

    /// The testable constructor: explicit root instead of the real HOME.
    pub fn create_under(root: &Path, cwd: &Path, model: &str) -> anyhow::Result<Self> {
        let header = SessionHeader::new(cwd, model);
        let name = format!("{}-{}.jsonl", file_stamp(), super::new_entry_id());
        let path = root.join(slug(cwd)).join(name);
        let header_line = serde_json::to_string(&header).context("serializing session header")?;
        Ok(Self {
            path,
            id: header.id,
            header_line,
            file: None,
            allow_empty_existing: false,
        })
    }

    /// Create a lazy session store at an explicit file path (`--session-file`).
    /// An absent file or a 0-byte existing file (e.g. `mktemp`) is initialized
    /// on first append; a non-empty existing file is refused.
    pub fn create_at(path: PathBuf, cwd: &Path, model: &str) -> anyhow::Result<Self> {
        match std::fs::metadata(&path) {
            Ok(meta) => {
                if meta.is_dir() {
                    anyhow::bail!("session file {} is a directory", path.display());
                }
                if meta.len() > 0 {
                    anyhow::bail!(
                        "session file {} already exists and is non-empty",
                        path.display()
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("inspecting {}", path.display())),
        }
        let header = SessionHeader::new(cwd, model);
        let header_line = serde_json::to_string(&header).context("serializing session header")?;
        Ok(Self {
            path,
            id: header.id,
            header_line,
            file: None,
            allow_empty_existing: true,
        })
    }

    /// Reopen an existing session file for appending (resume). The whole
    /// file is validated first — extending a corrupt file would bury the
    /// corruption under new entries. A torn write that dropped the final
    /// newline is healed (the newline re-appended) before validation: the
    /// last entry is intact data, and without the terminator the next
    /// append would glue two entries onto one line.
    pub fn open_existing(path: &Path) -> anyhow::Result<Self> {
        repair_missing_trailing_newline(path)?;
        let (header, _, warnings) = load(path)?;
        if !warnings.is_empty() {
            anyhow::bail!(
                "session {} has corrupt lines; not appending to it",
                path.display()
            );
        }
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        tighten(path);
        Ok(Self {
            path: path.to_path_buf(),
            id: header.id,
            header_line: String::new(),
            file: Some(file),
            allow_empty_existing: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session header's uuid — the hook payload's `session_id` (§22).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The header line as JSON — the record JSON mode echoes before the
    /// events (§15). Fresh stores still hold it in memory (the file
    /// materializes lazily); reopened stores re-read their first line.
    pub fn header_record(&self) -> anyhow::Result<String> {
        if !self.header_line.is_empty() {
            return Ok(self.header_line.clone());
        }
        let first = std::fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?
            .lines()
            .next()
            .with_context(|| format!("{} is empty", self.path.display()))?
            .to_string();
        Ok(first)
    }

    /// One entry per line, flushed. Errors propagate: session I/O is one of
    /// the run-aborting failure classes (§17).
    pub fn append(&mut self, entry: &Entry) -> anyhow::Result<()> {
        let line = serde_json::to_string(entry).context("serializing session entry")?;
        if self.file.is_none() {
            self.start_file()?;
        }
        let Some(file) = &mut self.file else {
            anyhow::bail!("session file at {} did not start", self.path.display());
        };
        writeln!(file, "{line}")
            .with_context(|| format!("appending to {}", self.path.display()))?;
        file.flush()
            .with_context(|| format!("flushing {}", self.path.display()))?;
        Ok(())
    }

    /// Materialize a fresh session file now rather than on the first
    /// append. `--watch` restarts resume by id, so the file must exist
    /// before the first message; plain runs keep creating nothing until
    /// they have something to say.
    pub fn materialize(&mut self) -> anyhow::Result<()> {
        if self.file.is_none() {
            self.start_file()?;
        }
        Ok(())
    }

    /// First append: create the directory and the file, write the header.
    fn start_file(&mut self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        #[cfg(unix)]
        opts.mode(0o600);
        opts.append(true);
        if self.allow_empty_existing {
            opts.create(true);
        } else {
            opts.create_new(true);
        }
        let mut file = opts
            .open(&self.path)
            .with_context(|| format!("creating {}", self.path.display()))?;
        tighten(&self.path);
        if self.allow_empty_existing
            && file
                .metadata()
                .with_context(|| format!("inspecting {}", self.path.display()))?
                .len()
                > 0
        {
            anyhow::bail!(
                "session file {} became non-empty before initialization",
                self.path.display()
            );
        }
        writeln!(file, "{}", self.header_line)
            .with_context(|| format!("writing {}", self.path.display()))?;
        file.flush()
            .with_context(|| format!("flushing {}", self.path.display()))?;
        self.file = Some(file);
        Ok(())
    }
}

/// Tighten session files to `0o600` on Unix so tool output (which may
/// capture secrets) is never world-readable.
fn tighten(_path: &Path) {
    #[cfg(unix)]
    let _ = std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o600));
}

/// Read a session file back: header, entries, and a warning per corrupt
/// line. Corrupt lines are skipped, not fatal (a torn tail is the crash
/// case §13 designs for); a missing or wrong header IS fatal.
pub fn load(path: &Path) -> anyhow::Result<(SessionHeader, Vec<Entry>, Vec<String>)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading session {}", path.display()))?;
    let mut lines = text.lines();
    let header: SessionHeader = serde_json::from_str(
        lines
            .next()
            .with_context(|| format!("{} is empty", path.display()))?,
    )
    .with_context(|| format!("{}: bad header", path.display()))?;
    if header.kind != "session" {
        anyhow::bail!("{}: first line is not a session header", path.display());
    }
    if header.version != 1 {
        anyhow::bail!(
            "{}: session format version {} is not supported (this build reads 1)",
            path.display(),
            header.version
        );
    }

    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue; // a trailing newline is not corruption
        }
        match serde_json::from_str(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => warnings.push(format!(
                "{}: line {} skipped (not valid JSON): {e}",
                path.display(),
                index + 2 // +2: header + 1-based
            )),
        }
    }
    Ok((header, entries, warnings))
}

#[cfg(test)]
mod tests;
