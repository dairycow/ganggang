//! A nudge back to the native tools when a `bash` call re-implements one.
//!
//! The model reaches for `cat`, `grep`, `find` and inline python inside a
//! shell command often enough to be worth one cheap string match: the cost
//! is a scan of the command text, the gain is a `[hint: ...]` line telling
//! it what to call next time. Nothing is refused, and a command that only
//! pipes one tool into another (`cargo test | tail -20`) stays untouched.

use std::path::Path;

const READ_HINT: &str =
    "use native read (supports offset/limit and negative offset), outline or edit instead of bash";
const GREP_HINT: &str = "use native grep instead of bash";
const FIND_HINT: &str = "use native find instead of bash";
const LS_HINT: &str = "use native ls or find instead of bash";
const PYTHON_HINT: &str = "use native read, edit or write instead of inline python in bash";
const CD_HINT: &str = "bash already runs in the working directory; omit cd";

/// Strip `#` comments and heredoc bodies (`<<EOF` ... `EOF`) outside quotes,
/// returning lines of shell code safe to split into command segments.
fn strip_comments_and_heredocs(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut lines = command.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let mut quote: Option<char> = None;
        let mut escaped = false;
        let mut comment_at: Option<usize> = None;
        let mut heredoc: Option<String> = None;
        let mut prev_ws = true;
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if escaped {
                escaped = false;
                prev_ws = false;
                i += 1;
                continue;
            }
            match quote {
                Some('\'') if b == b'\'' => quote = None,
                Some('\'') => {}
                Some(_) if b == b'\\' => escaped = true,
                Some(_) if b == b'"' => quote = None,
                Some(_) => {}
                None => match b {
                    b'\'' | b'"' => quote = Some(b as char),
                    b'\\' => escaped = true,
                    b'#' if prev_ws => {
                        comment_at = Some(i);
                        break;
                    }
                    b'<' if bytes.get(i + 1) == Some(&b'<') && bytes.get(i + 2) != Some(&b'<') => {
                        if let Some(delim) = parse_heredoc_delim(&line[i + 2..]) {
                            heredoc = Some(delim);
                        }
                    }
                    _ => {}
                },
            }
            prev_ws = b.is_ascii_whitespace() || matches!(b, b';' | b'&' | b'|' | b'(' | b')');
            i += 1;
        }
        match comment_at {
            Some(at) => {
                out.push_str(&line[..at]);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(line),
        }
        if let Some(delim) = heredoc {
            for body_line in lines.by_ref() {
                if body_line
                    .trim_end_matches(['\r', '\n'])
                    .trim_start_matches('\t')
                    == delim
                {
                    break;
                }
            }
        }
    }
    out
}

fn parse_heredoc_delim(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    if rest.is_empty() {
        return None;
    }
    let mut delim = String::new();
    let mut quote: Option<char> = None;
    for c in rest.chars() {
        match quote {
            Some(q) if c == q => break,
            Some(_) => delim.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_ascii_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>') => break,
            None => delim.push(c),
        }
    }
    (!delim.is_empty()).then_some(delim)
}

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
        if i < start {
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
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
/// Unquoted `|`, `<`, `>`, `(`, `)` act as word boundaries.
fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in segment.chars() {
        if escaped {
            word.push(c);
            started = true;
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
                c if c.is_whitespace() || matches!(c, '|' | '<' | '>' | '(' | ')' | '{' | '}') => {
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

/// The hint for a command word, with its args and raw segment for python.
fn hint_for(cmd: &str, args: &[String], segment: &str) -> Option<&'static str> {
    match cmd {
        "cat" | "head" | "tail" | "nl" | "sed" | "awk" => Some(READ_HINT),
        "grep" | "egrep" | "fgrep" | "rg" => Some(GREP_HINT),
        "find" | "fd" => Some(FIND_HINT),
        "ls" | "tree" => Some(LS_HINT),
        "python" | "python3"
            if args.iter().any(|a| a == "-c" || a == "-")
                && (segment.contains("open(")
                    || segment.contains(".write(")
                    || segment.contains("Path(")) =>
        {
            Some(PYTHON_HINT)
        }
        _ => None,
    }
}

/// The nudge for a command that duplicates a native tool, or `None` for a
/// normal shell command. A tool hint outranks a redundant `cd`.
pub(super) fn workaround_hint(command: &str, cwd: &Path) -> Option<&'static str> {
    let cleaned = strip_comments_and_heredocs(command);
    let cwd_str = cwd.to_string_lossy();
    let cwd_dir = cwd_str.trim_end_matches('/');
    let mut redundant_cd = false;
    let mut dir_changed = false;
    for segment in segments(&cleaned) {
        let words = words(segment);
        let mut at = 0;
        while words.get(at).is_some_and(|w| is_assignment(w)) {
            at += 1;
        }
        let Some(cmd) = words.get(at) else { continue };
        if cmd == "cd" {
            let mut arg_idx = at + 1;
            if words.get(arg_idx).is_some_and(|a| a == "--") {
                arg_idx += 1;
            }
            if let Some(arg) = words.get(arg_idx) {
                let trimmed = arg.trim_end_matches('/');
                let same_dir = trimmed == "." || trimmed == cwd_dir;
                if same_dir && !dir_changed {
                    redundant_cd = true;
                } else if !same_dir {
                    dir_changed = true;
                }
            }
            continue;
        }
        let base = cmd.rsplit('/').next().unwrap_or(cmd);
        if let Some(hint) = hint_for(base, &words[at + 1..], segment) {
            return Some(hint);
        }
    }
    if redundant_cd {
        return Some(CD_HINT);
    }
    None
}
