//! Path resolution and display helpers shared by the filesystem tools.

use std::path::{Path, PathBuf};

/// Expand a leading `~` or `~/...` against `home` when provided.
/// `~user/...` stays literal so we never guess another user's home.
pub(crate) fn expand_tilde(raw: &str, home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    if raw == "~" {
        return Some(home.to_path_buf());
    }
    raw.strip_prefix("~/").map(|rest| home.join(rest))
}

/// Resolve a tool path: expand a leading `~` or `~/...` against `HOME`
/// (when absolute, see [`crate::home::dir`]), then join against `cwd`.
pub(crate) fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    match expand_tilde(raw, crate::home::dir().as_deref()) {
        Some(expanded) => expanded,
        None => cwd.join(raw),
    }
}

/// Render a path for tool output without `.` noise: a search rooted at
/// `.` must report `sub/f.txt`, not `./sub/f.txt`. Lexical only — `..`
/// is preserved and the filesystem is not consulted.
pub(crate) fn display(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        if let std::path::Component::CurDir = component {
            continue;
        }
        out.push(component.as_os_str());
    }
    out.display().to_string()
}

pub(crate) fn display_relative(cwd: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let rendered = display(rel);
    if rendered.is_empty() {
        ".".to_string()
    } else {
        rendered
    }
}

pub(crate) fn resolve_walk_root(root: &Path) -> (PathBuf, bool) {
    let is_symlink = std::fs::symlink_metadata(root)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        (
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            true,
        )
    } else {
        (root.to_path_buf(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_expands_bare_and_slash_only() {
        let home = Path::new("/home/u");
        assert_eq!(
            expand_tilde("~", Some(home)),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(
            expand_tilde("~/.gg/config.toml", Some(home)),
            Some(PathBuf::from("/home/u/.gg/config.toml"))
        );
        assert_eq!(expand_tilde("~other/x", Some(home)), None);
        assert_eq!(expand_tilde("src/main.rs", Some(home)), None);
        assert_eq!(expand_tilde("~/.gg/config.toml", None), None);
    }
}
