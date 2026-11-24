//! A nudge back to the native tools when a `bash` call re-implements one.
//!
//! The model reaches for `cat`, `grep`, `find` and inline python inside a
//! shell command often enough to be worth one cheap string match: the cost
//! is a scan of the command text, the gain is a `[hint: ...]` line telling
//! it what to call next time. Nothing is refused, and a command that only
//! pipes one tool into another (`cargo test | tail -20`) stays untouched.

use std::path::Path;

const READ_HINT: &str =
    "use native read (supports offset/limit and negative offset), outline, or edit instead of bash";
const GREP_HINT: &str = "use native grep instead of bash";
const FIND_HINT: &str = "use native find instead of bash";
const LS_HINT: &str = "use native ls or find instead of bash";
const PYTHON_HINT: &str = "use native read, edit, or write instead of inline python in bash";
const CD_HINT: &str = "bash already runs in the working directory; omit cd";

/// Top-level command list segments: `&&`, `||`, `;` and newlines separate
/// them, quotes and backslash escapes do not. A single `|` is a pipe, so it
/// is not a separator and a pipeline is judged on its first command alone.
fn segments(command: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let bytes = command.as_bytes();
    for (i, c) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            // Inside single quotes everything is literal, backslash included.
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => {}
            Some(_) if c == '\\' => escaped = true,
            Some(_) if c == '"' => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => escaped = true,
                ';' | '\n' => {
                    out.push(&command[start..i]);
                    start = i + 1;
                }
                '&' | '|' if bytes.get(i + 1) == Some(&(c as u8)) => {
                    out.push(&command[start..i]);
                    start = i + 2;
                }
                _ => {}
            },
        }
    }
    out.push(&command[start..]);
    out
}

/// One segment split into words, with quotes and backslash escapes dropped.
fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in segment.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => word.push(c),
            Some(_) if c == '\\' => escaped = true,
            Some(_) if c == '"' => quote = None,
            Some(_) => word.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => escaped = true,
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                c => {
                    word.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        out.push(word);
    }
    out
}

/// `KEY=value` as it appears before a command name.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The hint for a command word, with the raw segment for the python case.
fn hint_for(cmd: &str, segment: &str) -> Option<&'static str> {
    match cmd {
        "cat" | "head" | "tail" | "nl" | "sed" | "awk" => Some(READ_HINT),
        "grep" | "egrep" | "fgrep" | "rg" => Some(GREP_HINT),
        "find" | "fd" => Some(FIND_HINT),
        "ls" | "tree" => Some(LS_HINT),
        "python" | "python3"
            if segment.contains("open(")
                || segment.contains(".write(")
                || segment.contains("Path(") =>
        {
            Some(PYTHON_HINT)
        }
        _ => None,
    }
}

/// The nudge for a command that duplicates a native tool, or `None` for a
/// normal shell command. A tool hint outranks a redundant `cd`.
pub(super) fn workaround_hint(command: &str, cwd: &Path) -> Option<&'static str> {
    let cwd_str = cwd.to_string_lossy();
    let cwd_dir = cwd_str.trim_end_matches('/');
    let mut redundant_cd = false;
    for segment in segments(command) {
        let words = words(segment);
        let mut at = 0;
        while words.get(at).is_some_and(|w| is_assignment(w)) {
            at += 1;
        }
        let Some(cmd) = words.get(at) else { continue };
        if cmd == "cd" {
            if words
                .get(at + 1)
                .is_some_and(|arg| arg.trim_end_matches('/') == cwd_dir)
            {
                redundant_cd = true;
            }
            continue;
        }
        let base = cmd.rsplit('/').next().unwrap_or(cmd);
        if let Some(hint) = hint_for(base, segment) {
            return Some(hint);
        }
    }
    if redundant_cd {
        return Some(CD_HINT);
    }
    None
}
