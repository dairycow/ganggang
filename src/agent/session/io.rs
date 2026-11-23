//! Session file I/O (spec §13): append-only JSONL under
//! `~/.gg/sessions/<path-slug>/`, one line per entry, flushed per append.
//!
//! Crash safety comes from the append-only shape: a torn trailing line is
//! skipped with a warning on load, never repaired in place. The raw file is
//! the source of truth — replay is `load` + [`super::to_wire`].

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::Entry;

/// The first line of a session file. `kind` is always `"session"`; it is
/// validated on load so a stray entry-line file cannot pose as a session.
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
/// first append. Launch-and-quit must not litter header-only files that
/// then outrank real sessions in `-c`'s newest-first listing.
pub struct SessionStore {
    path: PathBuf,
    id: String,
    header_line: String,
    file: Option<std::fs::File>,
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
        Ok(Self {
            path: path.to_path_buf(),
            id: header.id,
            header_line: String::new(),
            file: Some(file),
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
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("creating {}", self.path.display()))?;
        writeln!(file, "{}", self.header_line)
            .with_context(|| format!("writing {}", self.path.display()))?;
        file.flush()
            .with_context(|| format!("flushing {}", self.path.display()))?;
        self.file = Some(file);
        Ok(())
    }
}

/// A crash between a line's text and its newline leaves the file without a
/// trailing `\n`. Append it back through append-mode (which writes at the
/// end regardless of any stored offset) so the torn entry stays its own
/// line instead of concatenating with the next append.
fn repair_missing_trailing_newline(path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if matches!(bytes.last(), None | Some(b'\n')) {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} for repair", path.display()))?;
    file.write_all(b"\n")
        .and_then(|()| file.flush())
        .with_context(|| format!("repairing {}", path.display()))
}

/// Resume repair (§8: a tool_call is never replayed without its result):
/// assistant calls whose results never landed — a kill -9 or Ctrl+C
/// mid-batch — get synthesized `is_error` results, appended through the
/// store so disk and memory agree. Returns how many were synthesized.
pub fn repair_dangling_tool_calls(
    entries: &mut Vec<Entry>,
    store: &mut SessionStore,
) -> anyhow::Result<usize> {
    let answered: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message { message, .. } if message.role == super::Role::ToolResult => {
                message.tool_call_id.clone()
            }
            _ => None,
        })
        .collect();
    let dangling: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message { message, .. } if message.role == super::Role::Assistant => {
                Some(message.tool_calls.iter().map(|c| c.id.clone()))
            }
            _ => None,
        })
        .flatten()
        .filter(|id| !answered.contains(id))
        .collect();

    let repaired = dangling.len();
    for id in dangling {
        // One Entry, one id: appended to disk, then recorded in memory.
        let entry = Entry::Message {
            id: super::new_entry_id(),
            parent_id: entries.last().map(super::last_entry_id),
            timestamp: super::now_iso(),
            message: super::Message {
                role: super::Role::ToolResult,
                content: Some("aborted before execution".into()),
                tool_calls: Vec::new(),
                tool_call_id: Some(id),
                usage: None,
                is_error: true,
                reasoning: None,
            },
        };
        store.append(&entry)?;
        entries.push(entry);
    }
    Ok(repaired)
}

/// `~/.gg/sessions/<path-slug>` — where a cwd's sessions live. None
/// without HOME (no home, no sessions).
pub fn sessions_dir(cwd: &Path) -> Option<PathBuf> {
    sessions_root().map(|root| root.join(slug(cwd)))
}

/// `~/.gg/sessions` — the tree root only; [`SessionStore::create_under`]
/// adds the per-cwd slug itself.
pub fn sessions_root() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".gg").join("sessions"))
}

/// Filesystem-safe encoding of the cwd: `/` → `-`.
fn slug(cwd: &Path) -> String {
    cwd.display().to_string().replace('/', "-")
}

/// `YYYYMMDD-HHMMSS` in UTC. Second precision keeps two sessions started
/// within a minute sortable by name; within one second, the listing
/// falls back to mtime (still deterministic in practice).
fn file_stamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// One resumable session as the CLI sees it.
#[derive(Debug, Clone)]
pub struct SessionFile {
    pub id: String,
    pub timestamp: String,
    pub model: String,
    pub path: PathBuf,
}

/// All sessions stored under `dir`, newest first. Order: conversations
/// before header-only files (a `--watch` run materializes its file before
/// the first message; such a file must not outrank a real conversation in
/// `-c` or the `/resume` list, yet it stays resolvable by `--resume <id>`),
/// then filename stamp (descending — it encodes the start time), then mtime
/// for ties, then the name for full determinism. Files whose header cannot
/// be read are skipped with a warning — a session that cannot even present
/// a header is not resumable, but it must not hide the rest either.
pub fn list_sessions(dir: &Path) -> (Vec<SessionFile>, Vec<String>) {
    let mut sessions = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (sessions, warnings); // no dir yet: no sessions, that is normal
    };
    let paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    let stamp_of = |p: &Path| -> String {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.rsplit_once('-'))
            .map(|(stamp, _)| stamp.to_string())
            .unwrap_or_default()
    };
    let modified = |p: &Path| -> Option<std::time::SystemTime> {
        std::fs::metadata(p).and_then(|m| m.modified()).ok()
    };
    let mut found: Vec<(SessionFile, bool)> = Vec::new();
    for path in paths {
        match read_header(&path) {
            Ok((header, has_entries)) => found.push((
                SessionFile {
                    id: header.id,
                    timestamp: header.timestamp,
                    model: header.model,
                    path,
                },
                has_entries,
            )),
            Err(e) => warnings.push(format!("skipping session file: {e}")),
        }
    }
    found.sort_by(|(a, a_has), (b, b_has)| {
        b_has
            .cmp(a_has)
            .then_with(|| stamp_of(&b.path).cmp(&stamp_of(&a.path)))
            .then_with(|| modified(&b.path).cmp(&modified(&a.path)))
            .then_with(|| b.path.file_name().cmp(&a.path.file_name()))
    });
    sessions.extend(found.into_iter().map(|(session, _)| session));
    (sessions, warnings)
}

/// Parse just the first line of a session file, and report whether any
/// entry follows it.
fn read_header(path: &Path) -> anyhow::Result<(SessionHeader, bool)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines();
    let first = lines
        .next()
        .with_context(|| format!("{} is empty", path.display()))?;
    let header: SessionHeader =
        serde_json::from_str(first).with_context(|| format!("{}: bad header", path.display()))?;
    if header.kind != "session" || header.version != 1 {
        anyhow::bail!("{}: not a version-1 session file", path.display());
    }
    let has_entries = lines.any(|line| !line.trim().is_empty());
    Ok((header, has_entries))
}

/// Newest session in an explicit dir (the `-c` rule). Callers resolve the
/// dir via [`sessions_dir`].
pub fn latest_in(dir: &Path) -> Option<SessionFile> {
    let (sessions, _) = list_sessions(dir);
    sessions.into_iter().next()
}

/// The 8-hex id embedded in a session file's name — the handle users
/// actually see in paths and the `session:` line.
fn file_stem_id(session: &SessionFile) -> &str {
    session
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(_, id)| id)
        .unwrap_or("")
}

/// Resolve `--resume <id-or-prefix>` over an explicit listing: the prefix
/// may match the session's uuid id or the 8-hex id in its filename. Zero
/// or several matches are errors — several LIST the candidates so the user
/// can disambiguate.
pub fn resolve_prefix(sessions: &[SessionFile], prefix: &str) -> anyhow::Result<SessionFile> {
    let matches: Vec<&SessionFile> = sessions
        .iter()
        .filter(|s| s.id.starts_with(prefix) || file_stem_id(s).starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => anyhow::bail!("no session matches {prefix:?}"),
        many => anyhow::bail!(
            "ambiguous session prefix {prefix:?}: matches {}",
            many.iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Read a session file back: header, entries, and a warning per corrupt
/// line. A corrupt line anywhere is skipped, not fatal — a torn tail is the
/// crash case §13 designs for, and mid-file corruption has no better
/// recovery either. A missing or wrong header IS fatal: without it there is
/// no version guarantee about the lines below.
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
