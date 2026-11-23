//! Which program edits the draft, and how its command line is read.

/// The editor command: `$VISUAL`, else `$EDITOR`, else `nano` — the order
/// the other harnesses use, and pi's fallback. Empty counts as unset.
pub(super) fn editor_command(visual: Option<&str>, editor: Option<&str>) -> String {
    visual
        .into_iter()
        .chain(editor)
        .find(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "nano".into())
}

/// The editor command from the environment.
pub(super) fn configured() -> anyhow::Result<String> {
    resolve(|name| std::env::var(name))
}

/// [`editor_command`] over a variable lookup: the environment in use, a
/// fixed answer per test.
fn resolve(var: impl Fn(&str) -> Result<String, std::env::VarError>) -> anyhow::Result<String> {
    Ok(editor_command(
        value(&var, "VISUAL")?.as_deref(),
        value(&var, "EDITOR")?.as_deref(),
    ))
}

/// One variable of the lookup: blank counts as unset, and text that is not
/// UTF-8 is an error rather than a silent fallback.
fn value(
    var: &impl Fn(&str) -> Result<String, std::env::VarError>,
    name: &str,
) -> anyhow::Result<Option<String>> {
    match var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotUnicode(value)) => {
            anyhow::bail!("${name} is not valid UTF-8: {value:?}")
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
    }
}

/// Inside double quotes a backslash escapes only these.
const ESCAPABLE_IN_DOUBLE_QUOTES: &[char] = &['$', '`', '"', '\\'];

/// Split an editor command line into words the way a shell would: blanks
/// separate, quotes group, a backslash escapes the next character (only
/// [`ESCAPABLE_IN_DOUBLE_QUOTES`] inside double quotes). Enough for
/// `code --wait`, `emacsclient -t -a ""`, and a path with a space in it;
/// no variables, no globs. Empty when a quote is left open.
pub(super) fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\')
                if chars
                    .clone()
                    .next()
                    .is_some_and(|next| ESCAPABLE_IN_DOUBLE_QUOTES.contains(&next)) =>
            {
                word.push(chars.next().unwrap_or('\\'));
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, '\\') => {
                // Nothing left to escape: a literal backslash.
                word.push(chars.next().unwrap_or('\\'));
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Vec::new();
    }
    if started {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::VarError;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    #[test]
    fn visual_beats_editor_and_nano_is_the_fallback() {
        assert_eq!(editor_command(Some("vim"), Some("nano")), "vim");
        assert_eq!(editor_command(None, Some("code --wait")), "code --wait");
        assert_eq!(editor_command(Some(""), Some("nano")), "nano");
        assert_eq!(editor_command(Some("  "), None), "nano");
        assert_eq!(editor_command(None, None), "nano");
    }

    /// The same rule, read through a variable lookup, as production reads
    /// the environment.
    #[test]
    fn the_lookup_resolves_like_the_rule() {
        let none = |_: &str| Err(VarError::NotPresent);
        assert_eq!(resolve(none).unwrap(), "nano");
        let editor_only = |name: &str| {
            (name == "EDITOR")
                .then(|| "nvim".to_string())
                .ok_or(VarError::NotPresent)
        };
        assert_eq!(resolve(editor_only).unwrap(), "nvim");
        let both = |name: &str| match name {
            "VISUAL" => Ok("emacs -nw".to_string()),
            "EDITOR" => Ok("nvim".to_string()),
            _ => Err(VarError::NotPresent),
        };
        assert_eq!(resolve(both).unwrap(), "emacs -nw");
        // A variable that is not text is named, not treated as unset.
        let bad = |name: &str| {
            if name == "EDITOR" {
                Err(VarError::NotUnicode(OsString::from_vec(vec![0xff])))
            } else {
                Err(VarError::NotPresent)
            }
        };
        let error = resolve(bad).unwrap_err().to_string();
        assert!(error.contains("$EDITOR is not valid UTF-8"), "{error}");
    }

    #[test]
    fn command_lines_split_like_a_shell_would() {
        assert_eq!(split_command("nvim"), vec!["nvim"]);
        assert_eq!(split_command("  code  --wait  "), vec!["code", "--wait"]);
        assert_eq!(
            split_command("emacsclient -t -a \"\""),
            vec!["emacsclient", "-t", "-a", ""]
        );
        assert_eq!(
            split_command("'/opt/my editor' --wait"),
            vec!["/opt/my editor", "--wait"]
        );
        assert_eq!(split_command(r#"my\ editor arg"#), vec!["my editor", "arg"]);
        // No variables, no globs: the words are the value, verbatim.
        assert_eq!(
            split_command("sh -c '$EDITOR'"),
            vec!["sh", "-c", "$EDITOR"]
        );
        assert_eq!(
            split_command("sh -c \"$EDITOR \\\"x\\\"\""),
            vec!["sh", "-c", "$EDITOR \"x\""]
        );
        // Unusable lines come back empty rather than half-parsed.
        assert!(split_command("").is_empty());
        assert!(split_command("   ").is_empty());
        assert!(split_command("nvim -u \"unclosed").is_empty());
    }
}
