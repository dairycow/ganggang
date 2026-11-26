//! Session listing: discover, rank, and present resumable sessions.
//!
//! [`list_sessions`] scans one cwd's directory; [`list_all_sessions`] scans
//! every cwd under the sessions root. Both read only what a listing needs —
//! header, first user message, latest title — via the streaming
//! [`read_header`], so a long session never blocks `-c`.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;

use super::SessionHeader;

/// One resumable session as the CLI sees it.
#[derive(Debug, Clone)]
pub struct SessionFile {
    pub id: String,
    /// When the session file was written (from the header). Part of the
    /// listing's shape; the picker ages a row from its file name instead,
    /// so this stays unread here.
    #[allow(dead_code)]
    pub timestamp: String,
    pub model: String,
    /// The cwd the session ran in (from the header).
    pub cwd: String,
    /// The latest `"type":"title"` line's title, if any.
    pub title: Option<String>,
    /// The first non-empty user message, whitespace-collapsed and capped.
    pub preview: Option<String>,
    pub path: PathBuf,
}

/// `~/.gg/sessions/<path-slug>` — where a cwd's sessions live. None
/// without HOME (no home, no sessions).
pub fn sessions_dir(cwd: &Path) -> Option<PathBuf> {
    sessions_root().map(|root| root.join(slug(cwd)))
}

/// `~/.gg/sessions` — the tree root only; [`super::SessionStore::create_under`]
/// adds the per-cwd slug itself.
pub fn sessions_root() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".gg").join("sessions"))
}

/// Filesystem-safe encoding of the cwd: `/` → `-`.
pub(crate) fn slug(cwd: &Path) -> String {
    cwd.display().to_string().replace('/', "-")
}

/// `YYYYMMDD-HHMMSS` in UTC; within one second the listing falls back to
/// mtime (still deterministic in practice).
pub(crate) fn file_stamp() -> String {
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

/// What a listing scan needs: the header plus the display bits.
/// `has_entries` separates a real conversation from a `--watch` file with no messages yet.
#[derive(Debug)]
struct HeaderScan {
    header: SessionHeader,
    has_entries: bool,
    title: Option<String>,
    preview: Option<String>,
}

/// Stream a session file line by line, collecting what a listing needs.
/// Line 1 must parse and validate as a version-1 session header (fatal
/// otherwise — without it there is no version guarantee about the lines
/// below); later lines mark the file as a conversation on the first
/// non-empty line, take the preview from the first user message, and
/// update the title on every title line. Malformed lines are skipped
/// silently: a listing must not fail over a torn tail.
fn read_header(path: &Path) -> anyhow::Result<HeaderScan> {
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut header = None;
    let mut has_entries = false;
    let mut title = None;
    let mut preview = None;
    loop {
        line.clear();
        let len = reader
            .read_line(&mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if len == 0 {
            break;
        }
        let trimmed = line.trim();
        if header.is_none() {
            let parsed: SessionHeader = serde_json::from_str(trimmed)
                .with_context(|| format!("{}: bad header", path.display()))?;
            if parsed.kind != "session" || parsed.version != 1 {
                anyhow::bail!("{}: not a version-1 session file", path.display());
            }
            header = Some(parsed);
            continue;
        }
        if trimmed.is_empty() {
            continue; // a blank line is not an entry
        }
        has_entries = true;
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue; // malformed line: skip silently during the listing scan
        };
        if preview.is_none() {
            preview = extract_preview(&value);
        }
        title = extract_title(&value).or(title);
    }
    let Some(header) = header else {
        anyhow::bail!("{} is empty", path.display());
    };
    Ok(HeaderScan {
        header,
        has_entries,
        title,
        preview,
    })
}

/// The title of a `"type":"title"` line, when it carries a non-empty string.
fn extract_title(value: &Value) -> Option<String> {
    (value.get("type").is_some_and(|t| t == "title"))
        .then(|| value.get("title").and_then(Value::as_str))
        .flatten()
        .filter(|t| !t.is_empty())
        .map(String::from)
}

/// The preview of the first user message: whitespace-collapsed, capped at
/// 80 chars; synthetic `[aborted by user]`/`[continue …` markers never become a preview.
fn extract_preview(value: &Value) -> Option<String> {
    let message = value.get("message")?;
    if message
        .get("role")
        .is_none_or(|r| r.as_str() != Some("user"))
    {
        return None;
    }
    let content = message.get("content")?.as_str()?;
    if content.starts_with("[aborted by user]") || content.starts_with("[continue ") {
        return None;
    }
    let collapsed = collapse_whitespace(content);
    (!collapsed.is_empty()).then(|| truncate_preview(&collapsed))
}

/// Run every whitespace run to a single space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cap a preview at 80 chars, appending `…` when it was cut.
fn truncate_preview(text: &str) -> String {
    if text.chars().count() > 80 {
        let cut: String = text.chars().take(80).collect();
        format!("{cut}…")
    } else {
        text.to_string()
    }
}

/// Every `.jsonl` file under `dir`, newest first, with a warning per unreadable header.
fn list_dir(dir: &Path) -> (Vec<(SessionFile, bool)>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (found, warnings); // no dir yet: no sessions, that is normal
    };
    let paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    for path in paths {
        match read_header(&path) {
            Ok(scan) => {
                let HeaderScan {
                    header,
                    has_entries,
                    title,
                    preview,
                } = scan;
                found.push((
                    SessionFile {
                        id: header.id,
                        timestamp: header.timestamp,
                        model: header.model,
                        cwd: header.cwd,
                        title,
                        preview,
                        path,
                    },
                    has_entries,
                ));
            }
            Err(e) => warnings.push(format!("skipping session file: {e}")),
        }
    }
    sort_newest_first(&mut found);
    (found, warnings)
}

/// The listing order: conversations before header-only files (a `--watch`
/// run materializes its file before the first message; such a file must not
/// outrank a real conversation in `-c` or `/resume`, yet stays resolvable
/// by `--resume <id>`), then filename stamp descending, mtime, name.
fn sort_newest_first(found: &mut [(SessionFile, bool)]) {
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
    found.sort_by(|(a, a_has), (b, b_has)| {
        b_has
            .cmp(a_has)
            .then_with(|| stamp_of(&b.path).cmp(&stamp_of(&a.path)))
            .then_with(|| modified(&b.path).cmp(&modified(&a.path)))
            .then_with(|| b.path.file_name().cmp(&a.path.file_name()))
    });
}

/// All sessions stored under `dir`, newest first; unreadable headers are skipped with a warning.
pub fn list_sessions(dir: &Path) -> (Vec<SessionFile>, Vec<String>) {
    let (found, warnings) = list_dir(dir);
    let sessions = found.into_iter().map(|(session, _)| session).collect();
    (sessions, warnings)
}

/// Every session under the sessions root, merged across all cwd subdirectories, newest first.
pub fn list_all_sessions(root: &Path) -> (Vec<SessionFile>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return (Vec::new(), warnings); // no root yet: no sessions, that is normal
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let (sub, sub_warnings) = list_dir(&path);
            found.extend(sub);
            warnings.extend(sub_warnings);
        }
    }
    sort_newest_first(&mut found);
    let sessions = found.into_iter().map(|(session, _)| session).collect();
    (sessions, warnings)
}

/// Newest session in an explicit dir (the `-c` rule).
pub fn latest_in(dir: &Path) -> Option<SessionFile> {
    let (sessions, _) = list_sessions(dir);
    sessions.into_iter().next()
}

/// The 8-hex id in a session file's name: the handle users see, and the
/// one [`resolve_prefix`] and the `/resume` picker match a prefix against.
/// Empty for a file not named `<stamp>-<8hex>.jsonl`.
pub fn short_id(session: &SessionFile) -> &str {
    session
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .map(|(_, id)| id)
        .unwrap_or("")
}

/// The sessions a prefix names, in listing order: it may match the start
/// of the session's uuid id or of the 8-hex id in its filename. A caller
/// that wants to know whether anything matched, before deciding where
/// else to look, asks this.
pub fn matching_prefix<'a>(sessions: &'a [SessionFile], prefix: &str) -> Vec<&'a SessionFile> {
    sessions
        .iter()
        .filter(|s| s.id.starts_with(prefix) || short_id(s).starts_with(prefix))
        .collect()
}

/// Resolve `--resume <id-or-prefix>`: the prefix may match the session's uuid id
/// or the 8-hex id in its filename; zero or several matches are errors (several list candidates).
pub fn resolve_prefix(sessions: &[SessionFile], prefix: &str) -> anyhow::Result<SessionFile> {
    let matches = matching_prefix(sessions, prefix);
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
