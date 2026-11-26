//! What survives a restart: the arguments that rebuild the same run on the
//! same session, and the unsent draft saved beside the session file.
//!
//! The arguments are rewritten, never remembered: one original argv goes
//! in, the resumed form comes out. Everything below is pure or plainly
//! testable so the re-exec path is not only exercised by a live rebuild.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::Config;

/// What `main` execs, once the terminal is the shell's again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restart {
    pub binary: PathBuf,
    pub args: Vec<OsString>,
}

/// Save the composer draft (unless empty) and build the exec plan. The
/// draft is keyed by the session file, so it can never surface in another
/// conversation.
///
/// `cwd` is the directory the next process starts in — the launch
/// directory, where `--resume <id>` is resolved — not wherever a `/session`
/// has since moved the session. A session filed under another directory is
/// continued by its file instead.
pub fn restart_plan(
    cfg: &Config,
    cwd: &Path,
    session: Option<&Path>,
    draft: &str,
    original: &[OsString],
) -> anyhow::Result<Restart> {
    let session = session.context("--watch needs a session file to continue after a restart")?;
    anyhow::ensure!(
        session.exists(),
        "session file {} is gone; --watch cannot continue it (start a fresh one with /new)",
        session.display()
    );
    save_draft(session, draft)?;
    let in_sessions_dir = crate::agent::session::io::sessions_dir(cwd)
        .is_some_and(|dir| session.parent() == Some(dir.as_path()));
    let args = match session_id(session) {
        Ok(id) if in_sessions_dir && id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()) => {
            restart_args(original, &id)
        }
        _ => restart_args_with_file(original, session),
    };
    Ok(Restart {
        binary: cfg.binary.clone(),
        args,
    })
}

fn strip_session_flags(original: &[OsString]) -> Vec<OsString> {
    let mut args = Vec::with_capacity(original.len() + 2);
    let mut iter = original.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "-c" | "--continue" | "--no-session" => {}
            // clap takes both spellings; the value goes with the flag.
            "--resume" | "--session-file" => {
                iter.next();
            }
            _ if text.starts_with("--resume=") || text.starts_with("--session-file=") => {}
            _ => args.push(arg.clone()),
        }
    }
    args
}

/// The argv for the new process: the original minus session selection,
/// plus `--resume <id>`. `--watch` and every other flag survive, so the
/// restarted run keeps watching and stays the same kind of run.
pub fn restart_args(original: &[OsString], session_id: &str) -> Vec<OsString> {
    let mut args = strip_session_flags(original);
    args.push(OsString::from("--resume"));
    args.push(OsString::from(session_id));
    args
}

fn restart_args_with_file(original: &[OsString], session: &Path) -> Vec<OsString> {
    let mut args = strip_session_flags(original);
    args.push(OsString::from("--session-file"));
    args.push(session.as_os_str().to_os_string());
    args
}

/// Rewrite `--provider`/`-P` and `--model`/`-m` in `args` so a mid-session
/// `/model` switch survives a `--watch` restart.
///
/// The endpoint goes in as one `--model provider/model` spec. Its first
/// segment is the provider and the rest is the model, however many slashes
/// the id carries. Passed apart, a vendor prefix that is also a provider
/// name (`-P openrouter -m deepseek/...`) would name a second provider.
pub fn apply_endpoint_args(args: Vec<OsString>, provider: &str, model: &str) -> Vec<OsString> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "-P" | "--provider" | "-m" | "--model" => {
                iter.next();
            }
            _ if text.starts_with("--provider=") || text.starts_with("--model=") => {}
            _ => out.push(arg),
        }
    }
    out.push(OsString::from("--model"));
    out.push(OsString::from(format!("{provider}/{model}")));
    out
}

/// The 8-hex id in a session file's name (`<stamp>-<id>.jsonl`) — the
/// handle `--resume` accepts.
pub fn session_id(session: &Path) -> anyhow::Result<String> {
    let stem = session
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("session file has no readable name")?;
    let (_, id) = stem
        .rsplit_once('-')
        .context("session file name has no id segment")?;
    anyhow::ensure!(!id.is_empty(), "session file name has an empty id");
    Ok(id.to_string())
}

/// Beside the session, so it lives and dies with the conversation. The
/// sessions tree is ordinary-permission (0755/0644); the draft's own 0600
/// is what keeps it private.
fn draft_path(session: &Path) -> PathBuf {
    session.with_extension("watch-draft")
}

/// Write the unsent composer text for the next process; empty text clears
/// a stale draft instead of creating an empty file.
pub fn save_draft(session: &Path, text: &str) -> anyhow::Result<()> {
    let path = draft_path(session);
    if text.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("clearing {}", path.display())),
        }
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("saving the draft to {}", path.display()))?;
    // `mode` only applies when the file is created; a stale draft must not
    // keep looser permissions it was left with.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("securing the draft at {}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .with_context(|| format!("saving the draft to {}", path.display()))
}

/// Read and remove the draft an earlier process left for this session.
pub fn take_draft(session: &Path) -> Option<String> {
    let path = draft_path(session);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn restart_args_replace_any_session_selection() {
        let original = argv(&["--watch", "--tools", "read,bash", "-c", "--model", "x"]);
        assert_eq!(
            restart_args(&original, "abcd1234"),
            argv(&[
                "--watch",
                "--tools",
                "read,bash",
                "--model",
                "x",
                "--resume",
                "abcd1234"
            ])
        );
    }

    #[test]
    fn restart_args_drop_both_resume_spellings_and_no_session() {
        for original in [
            argv(&["--watch", "--resume", "old", "--verbose"]),
            argv(&["--watch", "--resume=old", "--verbose"]),
            argv(&["--watch", "--no-session", "--verbose"]),
        ] {
            assert_eq!(
                restart_args(&original, "deadbeef"),
                argv(&["--watch", "--verbose", "--resume", "deadbeef"])
            );
        }
    }

    #[test]
    fn session_id_is_the_hex_segment_of_the_file_name() {
        let id = session_id(Path::new(
            "/home/user/.gg/sessions/-home-user-proj/20261123-161234-abcd1234.jsonl",
        ))
        .unwrap();
        assert_eq!(id, "abcd1234");
        assert!(session_id(Path::new("nohyphen.jsonl")).is_err());
    }

    #[test]
    fn draft_round_trips_and_is_removed_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("20261123-161234-abcd1234.jsonl");
        std::fs::write(&session, "{}\n").unwrap();
        save_draft(&session, "unsent message").unwrap();
        assert_eq!(take_draft(&session).as_deref(), Some("unsent message"));
        assert_eq!(take_draft(&session), None, "taking twice yields nothing");

        save_draft(&session, "stale").unwrap();
        save_draft(&session, "").unwrap();
        assert_eq!(take_draft(&session), None, "empty text clears the draft");
    }

    #[test]
    fn draft_is_private_and_whitespace_is_carried() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("20261123-161234-abcd1234.jsonl");
        std::fs::write(&session, "{}\n").unwrap();
        save_draft(&session, " \n ").unwrap();
        let mode = std::fs::metadata(draft_path(&session))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(take_draft(&session).as_deref(), Some(" \n "));
    }

    #[test]
    fn plan_requires_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: dir.path().to_path_buf(),
            profile: super::super::config::Profile::Debug,
            binary: dir.path().join("gg"),
            roots: Vec::new(),
        };
        assert!(restart_plan(&cfg, dir.path(), None, "", &[]).is_err());

        let missing = dir.path().join("20261123-161234-abcd1234.jsonl");
        assert!(
            restart_plan(&cfg, dir.path(), Some(&missing), "", &[]).is_err(),
            "a vanished session cannot be resumed"
        );
    }

    #[test]
    fn apply_endpoint_args_replaces_provider_and_model_flags() {
        let original = argv(&[
            "--watch",
            "-P",
            "strata",
            "--model=old",
            "--resume",
            "abcd1234",
        ]);
        assert_eq!(
            apply_endpoint_args(original, "deepseek", "deepseek-flash"),
            argv(&[
                "--watch",
                "--resume",
                "abcd1234",
                "--model",
                "deepseek/deepseek-flash",
            ])
        );
    }

    #[test]
    fn apply_endpoint_args_resolve_back_to_the_same_endpoint() {
        // The new process reads the spec through `apply_cli`, which takes
        // the first segment as the provider, so a vendor prefix that is
        // also a provider name has to stay part of the model.
        let original = argv(&["--watch", "-P", "strata"]);
        for (provider, model) in [
            ("deepseek", "deepseek-flash"),
            ("openrouter", "upstage/solar-mini-4-5"),
            ("openrouter", "deepseek/deepseek-v4.1-flash"),
        ] {
            let spec = apply_endpoint_args(original.clone(), provider, model)
                .pop()
                .and_then(|arg| arg.into_string().ok())
                .unwrap();
            let mut cfg = crate::config::Config::default();
            cfg.apply_cli(None, Some(&spec)).unwrap();
            let ep = cfg.active_endpoint();
            assert_eq!(ep.provider, provider, "{spec}");
            assert_eq!(ep.model, model, "{spec}");
        }
    }

    #[test]
    fn restart_args_strips_session_file_and_plan_falls_back_for_custom_names() {
        let original = argv(&["--watch", "--session-file", "/tmp/custom.jsonl"]);
        assert_eq!(
            restart_args(&original, "deadbeef"),
            argv(&["--watch", "--resume", "deadbeef"])
        );

        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            root: dir.path().to_path_buf(),
            profile: super::super::config::Profile::Debug,
            binary: dir.path().join("gg"),
            roots: Vec::new(),
        };
        let hyphenated = dir.path().join("my-custom-session.jsonl");
        std::fs::write(&hyphenated, "{}\n").unwrap();
        let plan1 = restart_plan(&cfg, dir.path(), Some(&hyphenated), "", &original).unwrap();
        assert_eq!(
            plan1.args,
            argv(&[
                "--watch",
                "--session-file",
                hyphenated.to_string_lossy().as_ref()
            ])
        );

        let external_hex = dir.path().join("20261123-161234-abcd1234.jsonl");
        std::fs::write(&external_hex, "{}\n").unwrap();
        let plan2 = restart_plan(&cfg, dir.path(), Some(&external_hex), "", &original).unwrap();
        assert_eq!(
            plan2.args,
            argv(&[
                "--watch",
                "--session-file",
                external_hex.to_string_lossy().as_ref()
            ])
        );
    }
}
