//! The draft: a file in a private directory under the temp dir, written
//! before the editor starts and read back once it has ended.

use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context as _;

/// This run's draft directory, once chosen.
static CHOSEN: OnceLock<PathBuf> = OnceLock::new();

/// Where this run's drafts live: together in one private directory, so an
/// editor with persistent undo keeps one history per gg run rather than
/// one per Ctrl+G. Chosen once.
pub(super) fn dir() -> io::Result<PathBuf> {
    if let Some(dir) = CHOSEN.get() {
        return Ok(dir.clone());
    }
    // The first name is predictable, and a shared temp dir is everyone's:
    // it is taken only if it is this user's to change. Anything else there
    // — another user's directory, a planted link — and the run takes a
    // name of its own.
    let dir = claim(&plain_path()).or_else(|_| {
        claim(&std::env::temp_dir().join(format!("gg-draft-{}", uuid::Uuid::new_v4())))
    })?;
    let _ = CHOSEN.set(dir.clone());
    Ok(dir)
}

/// The run's draft directory — once chosen, else the name it would try
/// first (for a message).
pub(super) fn dir_path() -> PathBuf {
    CHOSEN.get().cloned().unwrap_or_else(plain_path)
}

fn plain_path() -> PathBuf {
    std::env::temp_dir().join(format!("gg-draft-{}", std::process::id()))
}

/// A directory readable by the user alone at `candidate`: created, or —
/// already there — taken only if it is a directory and not a link (a link
/// would put the drafts wherever it points) and this user may change its
/// mode, which for anyone else's is refused. Nothing is changed before
/// those hold.
fn claim(candidate: &Path) -> io::Result<PathBuf> {
    match DirBuilder::new().mode(0o700).create(candidate) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    if !std::fs::symlink_metadata(candidate)?.file_type().is_dir() {
        return Err(io::Error::other("not a directory"));
    }
    std::fs::set_permissions(candidate, Permissions::from_mode(0o700))?;
    Ok(candidate.to_path_buf())
}

/// A new draft holding `text` in `dir`; its path. Readable by the user
/// alone, and under the first name that is free — one kept from a failed
/// session is never written over.
pub(super) fn write(dir: &Path, text: &str) -> anyhow::Result<PathBuf> {
    let (path, mut file) =
        open(dir).with_context(|| format!("writing a draft in {}", dir.display()))?;
    if let Err(e) = file.write_all(text.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(anyhow::Error::new(e).context("writing the draft"));
    }
    Ok(path)
}

fn open(dir: &Path) -> io::Result<(PathBuf, File)> {
    for n in 1..1000 {
        let name = if n == 1 {
            "draft.md".to_string()
        } else {
            format!("draft-{n}.md")
        };
        let path = dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(
        "the draft directory is full of drafts kept from failed sessions",
    ))
}

/// Remove the draft directory on the way out. Only an empty one goes: a
/// draft kept for the user stays.
pub(super) fn cleanup() {
    let _ = std::fs::remove_dir(dir_path());
}

/// A saved draft as composer text. Line endings normalize the way a
/// paste's do, a byte-order mark is dropped (it is not text), and the
/// final newline goes: editors write one per line, and the last one closes
/// the last line rather than starting a new one.
pub(super) fn composer_text(raw: &str) -> String {
    let text = raw
        .strip_prefix('\u{feff}')
        .unwrap_or(raw)
        .replace("\r\n", "\n");
    text.strip_suffix('\n')
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(&text)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_drafts_become_composer_text() {
        assert_eq!(composer_text("one\r\ntwo\r\n"), "one\ntwo");
        assert_eq!(composer_text("hello"), "hello");
        // Exactly the final newline goes: the ones before it are lines the
        // user left empty on purpose.
        assert_eq!(composer_text("a\n\nb\n\n"), "a\n\nb\n");
        assert_eq!(composer_text("\n\n"), "\n");
        assert_eq!(composer_text("\u{feff}hi\n"), "hi");
        assert_eq!(composer_text(""), "");
    }

    #[test]
    fn drafts_take_the_first_free_name_and_are_private() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            write(dir.path(), "first").unwrap(),
            dir.path().join("draft.md")
        );
        assert_eq!(
            write(dir.path(), "second").unwrap(),
            dir.path().join("draft-2.md"),
            "a kept draft is not written over"
        );
        std::fs::remove_file(dir.path().join("draft.md")).unwrap();
        assert_eq!(
            write(dir.path(), "third").unwrap(),
            dir.path().join("draft.md"),
            "free again"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("draft.md")).unwrap(),
            "third"
        );

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join("draft.md")), 0o600);

        // A claimed directory is the user's alone, and asking twice is
        // fine; one left too open is tightened on the way in.
        let ours = claim(&dir.path().join("run")).unwrap();
        assert_eq!(mode(&ours), 0o700);
        assert_eq!(claim(&ours).unwrap(), ours);
        let open = dir.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, Permissions::from_mode(0o755)).unwrap();
        assert_eq!(claim(&open).unwrap(), open);
        assert_eq!(mode(&open), 0o700);
    }

    /// A name already taken by something that is not a directory is not
    /// claimed, whoever owns it: the run moves to a name of its own.
    #[test]
    fn a_taken_name_is_not_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        let taken = dir.path().join("taken");
        std::fs::write(&taken, "a file").unwrap();
        assert!(claim(&taken).is_err());
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "a file");
        assert_eq!(mode(&taken), 0o644, "and it was not touched");
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        assert!(claim(&dangling).is_err());
        // Nor is a link to a directory of this user's: the drafts would
        // land wherever it points.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(claim(&link).is_err());
        assert_eq!(mode(&elsewhere), 0o755, "the target was not touched");
    }

    /// A directory gg cannot write in is named, not reported as a bare
    /// "permission denied".
    #[test]
    fn an_unwritable_draft_directory_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let nowhere = dir.path().join("no-such-dir");
        let error = write(&nowhere, "text").unwrap_err().to_string();
        assert!(error.contains(&nowhere.display().to_string()), "{error}");
    }
}
