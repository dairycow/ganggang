//! Structural extraction: one scanner per language family, chosen by file
//! extension, with a keyword fallback for everything else. No new
//! dependencies. The brace families share [`brace::brace_scan`], which names
//! the item each line opens and spans it to its closing brace or `;`; the Rust
//! scanner first blanks comments and string bodies so braces inside them do
//! not count. Markdown and TOML share `sections`, which runs an entry to the
//! line before the next heading or table header, and Python is read by
//! indentation.

use std::path::Path;

use super::super::Cancel;
use super::brace::{
    Line, brace_scan, c_kind, fallback_kind, go_kind, js_kind, rust_code, rust_kind, shell_kind,
};

/// One symbol: inclusive 1-based line span, nesting level, signature text.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub start: usize,
    pub end: usize,
    pub depth: usize,
    pub sig: String,
}

/// A stopped scan answers None: the entries found so far are not the truth.
pub(super) type Found = Option<Vec<Entry>>;

/// The stop request, checked every 256 lines: cheap in a hot loop, prompt
/// enough that a large file stops in well under a second.
pub(super) fn stopped(cancel: &Cancel, i: usize) -> bool {
    i.is_multiple_of(256) && cancel.is_cancelled()
}

/// The leading identifier of `s`, empty when it starts with anything else.
pub(super) fn word(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// `s` with its leading identifier `w` removed and whitespace trimmed.
pub(super) fn after<'a>(s: &'a str, w: &str) -> &'a str {
    s[w.len()..].trim_start()
}

/// Section files: Markdown headings and TOML tables. `header` names a
/// heading line with its depth; an entry runs to the line before the next
/// heading of its own level or a shallower one, or to the end of the file.
fn sections(
    lines: &[&str],
    cancel: &Cancel,
    header: impl Fn(&str) -> Option<usize>,
    fenced: bool,
) -> Found {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if stopped(cancel, i) {
            return None;
        }
        let depth = match header(lines[i]) {
            Some(depth) => depth,
            None => {
                i = fence_end(lines, i, fenced);
                continue;
            }
        };
        let mut end = lines.len() - 1;
        let mut j = i + 1;
        while j < lines.len() {
            if let Some(level) = header(lines[j])
                && level <= depth
            {
                end = j - 1;
                break;
            }
            j = fence_end(lines, j, fenced);
        }
        entries.push(Entry {
            start: i + 1,
            end: end + 1,
            depth,
            sig: lines[i].trim().to_string(),
        });
        i += 1;
    }
    Some(entries)
}

/// The line to look at after `j`: past a fenced code block in Markdown, the
/// next line everywhere else.
fn fence_end(lines: &[&str], j: usize, fenced: bool) -> usize {
    if j >= lines.len() {
        return j;
    }
    let trimmed = lines[j].trim_start();
    if !fenced || !(trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
        return j + 1;
    }
    let mark = if trimmed.starts_with("```") {
        "```"
    } else {
        "~~~"
    };
    let mut k = j + 1;
    while k < lines.len() && !lines[k].trim_start().starts_with(mark) {
        k += 1;
    }
    k + 1
}

/// The ATX heading level of a line, 0 when it is not a heading. The `#` run
/// is followed by whitespace or nothing, so a `#tag` is not a heading.
fn heading_level(line: &str) -> usize {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    let rest = &trimmed[hashes..];
    let spaced = rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace());
    if hashes > 0 && spaced {
        hashes.min(6)
    } else {
        0
    }
}

/// A Markdown heading: levels 1 to 4, nested two levels deep at most.
fn md_header(line: &str) -> Option<usize> {
    let level = heading_level(line);
    (level > 0 && level <= 4).then(|| (level - 1).min(2))
}

/// A TOML or INI table header: `[section]` or `[[array of tables]]`.
fn toml_header(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    let table = (trimmed.starts_with("[[") && trimmed.ends_with("]]") && trimmed.len() > 3)
        || (trimmed.starts_with('[')
            && !trimmed.starts_with("[[")
            && trimmed.ends_with(']')
            && trimmed.len() > 1);
    table.then_some(0)
}

/// Leading whitespace of a Python line; a tab counts as eight columns.
fn indent_of(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 8 } else { 1 })
        .sum()
}

/// A Python `def`, `class` or `async def`, nested by its indentation.
fn py_header(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    let rest = trimmed
        .strip_prefix("async")
        .unwrap_or(trimmed)
        .trim_start();
    let w = word(rest);
    (w == "def" || w == "class").then(|| indent_of(line) / 4)
}

/// Python's body rule: the entry ends at the last following line that is
/// blank, commented, or more indented than the declaration.
fn python_end(lines: &[&str], start: usize) -> usize {
    let own = indent_of(lines[start]);
    let mut end = start;
    for (j, line) in lines.iter().enumerate().skip(start + 1) {
        let next = line.trim_start();
        if next.is_empty() || next.starts_with('#') {
            continue;
        }
        if indent_of(line) <= own {
            break;
        }
        end = j;
    }
    end
}

/// Python: declarations by indentation, bodies by deeper indentation.
fn python(text: &str, cancel: &Cancel) -> Found {
    let lines: Vec<&str> = text.lines().collect();
    let mut entries = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if stopped(cancel, i) {
            return None;
        }
        let Some(depth) = py_header(line) else {
            continue;
        };
        entries.push(Entry {
            start: i + 1,
            end: python_end(&lines, i) + 1,
            depth,
            sig: line.trim_start().to_string(),
        });
    }
    Some(entries)
}

/// The entry list for `path`, or None when the user stopped the run.
pub(super) fn extract(path: &Path, text: &str, cancel: &Cancel) -> Found {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let fenced = matches!(ext.as_str(), "md" | "mdx");
    if fenced || matches!(ext.as_str(), "toml" | "ini") {
        let lines: Vec<&str> = text.lines().collect();
        let header = if fenced { md_header } else { toml_header };
        return sections(&lines, cancel, header, fenced);
    }
    if matches!(ext.as_str(), "py" | "pyi") {
        return python(text, cancel);
    }
    let clean: Vec<String>;
    let lines: Vec<&str> = if ext == "rs" {
        clean = rust_code(text);
        clean.iter().map(String::as_str).collect()
    } else {
        text.lines().collect()
    };
    let classify: fn(&Line) -> Option<&'static str> = match ext.as_str() {
        "rs" => rust_kind,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => js_kind,
        "go" => go_kind,
        "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "cs" => c_kind,
        "sh" | "bash" | "zsh" => shell_kind,
        _ => fallback_kind,
    };
    brace_scan(&lines, cancel, classify)
}
