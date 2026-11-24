//! A nudge back to the native tools when a `bash` call re-implements one.
//!
//! The model reaches for `cat`, `grep`, `find` and inline python or perl
//! inside a shell command often enough to be worth one cheap string match:
//! the cost is a scan of the command text, the gain is a `[hint: ...]` line
//! telling it what to call next time. Nothing is refused, and a command that
//! only pipes one tool into another (`cargo test | tail -20`) stays
//! untouched.

use std::path::Path;

const READ_HINT: &str =
    "use native read (supports offset/limit and negative offset), outline or edit instead of bash";
const GREP_HINT: &str = "use native grep instead of bash";
const FIND_HINT: &str = "use native find instead of bash";
const LS_HINT: &str = "use native ls or find instead of bash";
const PYTHON_HINT: &str = "use native read, edit or write instead of inline python in bash";
const PERL_HINT: &str =
    "use native edit (supports replace_all: true) instead of inline perl in bash";
const CD_HINT: &str = "bash already runs in the working directory; omit cd";

/// Markers of python code that touches file contents, looked for in the
/// command line and in the heredoc body that follows the call.
const PYTHON_FILE_MARKERS: [&str; 5] = ["open(", ".write(", ".read_text(", ".write_text(", "Path("];

/// `perl` short options that take a value, so a cluster such as `-I/tmp/lib`
/// has no in-place `i` in it.
const PERL_VALUE_OPTS: [char; 5] = ['e', 'E', 'I', 'M', 'm'];

/// Strip `#` comments and heredoc bodies (`<<EOF` ... `EOF`) outside quotes,
/// returning one `(cleaned line, heredoc bodies)` pair per source line so a
/// heredoc body still counts as part of the command that opened it.
fn strip_comments_and_heredocs(command: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
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
        let cleaned = match comment_at {
            Some(at) => {
                let mut head = line[..at].to_string();
                if line.ends_with('\n') {
                    head.push('\n');
                }
                head
            }
            None => line.to_string(),
        };
        // Body lines leave the shell code but stay attached to the line that
        // opened the heredoc.
        let mut bodies = String::new();
        if let Some(delim) = heredoc {
            for body_line in lines.by_ref() {
                if body_line
                    .trim_end_matches(['\r', '\n'])
                    .trim_start_matches('\t')
                    == delim
                {
                    break;
                }
                bodies.push_str(body_line);
            }
        }
        out.push((cleaned, bodies));
    }
    out
}

/// A line whose last character is an unescaped `\` continues on the next one.
fn ends_with_unescaped_backslash(line: &str) -> bool {
    let backslashes = line
        .trim_end_matches(['\n', '\r'])
        .chars()
        .rev()
        .take_while(|c| *c == '\\')
        .count();
    backslashes % 2 == 1
}

/// Glue continuation lines back together so a command split over lines stays
/// one segment, with the heredoc bodies of all of its lines kept together.
fn join_line_continuations(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut text = String::new();
    let mut bodies = String::new();
    for (line, body) in pairs {
        text.push_str(&line);
        bodies.push_str(&body);
        if ends_with_unescaped_backslash(&text) {
            continue;
        }
        out.push((std::mem::take(&mut text), std::mem::take(&mut bodies)));
    }
    if !text.is_empty() || !bodies.is_empty() {
        out.push((text, bodies));
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

/// A `perl` argument that turns on in-place editing: a bare `-i`, or a short
/// flag cluster such as `-0pi` or `-pi.bak` whose `i` precedes any
/// value-taking option.
fn is_perl_in_place(arg: &str) -> bool {
    let Some(flags) = perl_short_flags(arg) else {
        return false;
    };
    for c in flags.chars() {
        if c == 'i' {
            return true;
        }
        if PERL_VALUE_OPTS.contains(&c) {
            return false;
        }
    }
    false
}

/// The flag letters of a single-dash `perl` option cluster.
fn perl_short_flags(arg: &str) -> Option<&str> {
    match arg.strip_prefix('-') {
        Some(flags) if !flags.starts_with('-') && !flags.is_empty() => Some(flags),
        _ => None,
    }
}

/// An in-place `perl` flag paired with a script to run over the file.
fn perl_edits_in_place(args: &[String]) -> bool {
    let has_in_place = args.iter().any(|a| is_perl_in_place(a));
    let has_script_flag = args.iter().any(|a| {
        let Some(flags) = perl_short_flags(a) else {
            return false;
        };
        for c in flags.chars() {
            if matches!(c, 'e' | 'E' | 'p' | 'n') {
                return true;
            }
            if PERL_VALUE_OPTS.contains(&c) {
                return false;
            }
        }
        false
    });
    has_in_place && has_script_flag
}

/// The hint for a command word, with its args, its raw segment and the
/// heredoc body attached to that line.
fn hint_for(cmd: &str, args: &[String], segment: &str, heredoc_body: &str) -> Option<&'static str> {
    match cmd {
        "cat" | "head" | "tail" | "nl" | "sed" | "awk" => Some(READ_HINT),
        "grep" | "egrep" | "fgrep" | "rg" => Some(GREP_HINT),
        "find" | "fd" => Some(FIND_HINT),
        "ls" | "tree" => Some(LS_HINT),
        "python" | "python3"
            if (args.iter().any(|a| a == "-c")
                && PYTHON_FILE_MARKERS.iter().any(|m| segment.contains(m)))
                || ((args.iter().any(|a| a == "-") || segment.contains("<<"))
                    && PYTHON_FILE_MARKERS.iter().any(|m| heredoc_body.contains(m))) =>
        {
            Some(PYTHON_HINT)
        }
        "perl" if perl_edits_in_place(args) => Some(PERL_HINT),
        _ => None,
    }
}

/// The nudge for a command that duplicates a native tool, or `None` for a
/// normal shell command. A tool hint outranks a redundant `cd`.
pub(super) fn workaround_hint(command: &str, cwd: &Path) -> Option<&'static str> {
    let cwd_str = cwd.to_string_lossy();
    let cwd_dir = cwd_str.trim_end_matches('/');
    let mut redundant_cd = false;
    let mut dir_changed = false;
    for (line, heredoc_body) in join_line_continuations(strip_comments_and_heredocs(command)) {
        for segment in segments(&line) {
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
            if let Some(hint) = hint_for(base, &words[at + 1..], segment, &heredoc_body) {
                return Some(hint);
            }
        }
    }
    if redundant_cd {
        return Some(CD_HINT);
    }
    None
}
