//! The user's home directory, absolute or nothing.
//!
//! `HOME` locates user-level policy and state (`~/.gg/hooks.toml`, config,
//! sessions, prompt templates, skills, `AGENTS.md`). A relative or empty
//! value must not turn those into cwd-relative paths — a cloned repository
//! could then supply executable hooks or a config that disables a guard.
//! Only an absolute path counts as a home.

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn dir() -> Option<PathBuf> {
    from(std::env::var_os("HOME"))
}

pub(crate) fn from(var: Option<OsString>) -> Option<PathBuf> {
    var.map(PathBuf::from).filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_absolute_home_counts() {
        assert_eq!(
            from(Some(OsString::from("/home/u"))),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(from(Some(OsString::from("relative"))), None);
        assert_eq!(from(Some(OsString::from(""))), None);
        assert_eq!(from(None), None);
    }
}
