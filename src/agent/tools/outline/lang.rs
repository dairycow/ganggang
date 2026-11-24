//! Structural extraction: dispatch by extension across brace languages,
//! Python, Markdown, and TOML/INI.

use std::path::Path;

use super::super::Cancel;
use super::brace::{Line, brace_scan, c_kind, fallback_kind, go_kind, js_kind, shell_kind};
use super::rust::{rust_code, rust_kind};

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

/// The stop request, checked every 256 lines.
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

/// Section files: Markdown headings and TOML tables.
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
    if !fenced {
        return j + 1;
    }
    let fence_ch = if trimmed.starts_with("```") {
        '`'
    } else if trimmed.starts_with("~~~") {
        '~'
    } else {
        return j + 1;
    };
    let count = trimmed.chars().take_while(|&c| c == fence_ch).count();
    let info = &trimmed[count..];
    if fence_ch == '`' && info.contains('`') {
        return j + 1;
    }
    let mut k = j + 1;
    while k < lines.len() {
        let t = lines[k].trim();
        let close_count = t.chars().take_while(|&c| c == fence_ch).count();
        if close_count >= count && t[close_count..].trim().is_empty() {
            return k + 1;
        }
        k += 1;
    }
    lines.len()
}

/// The ATX heading level of a line, 0 when it is not a heading.
fn heading_level(line: &str) -> usize {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return 0;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes > 6 {
        return 0;
    }
    match trimmed[hashes..].chars().next() {
        Some(c) if c.is_whitespace() => hashes,
        _ => 0,
    }
}

/// A Markdown heading: levels 1 to 4 (depth 0 to 3; `outline.rs` caps indent).
fn md_header(line: &str) -> Option<usize> {
    let level = heading_level(line);
    (level > 0 && level <= 4).then(|| level - 1)
}

/// Strip an unquoted `#` trailing comment from a TOML line.
fn strip_toml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => {}
            Some('"') if c == '\\' => escaped = true,
            Some('"') if c == '"' => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' => return &line[..i],
            None => {}
        }
    }
    line
}

/// A TOML or INI table header: `[section]` or `[[array of tables]]`.
fn toml_header(line: &str) -> Option<usize> {
    let trimmed = strip_toml_comment(line).trim();
    let inner = if let Some(rest) = trimmed
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
    {
        rest
    } else if !trimmed.starts_with("[[")
        && let Some(rest) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
    {
        rest
    } else {
        return None;
    };
    let inner = inner.trim();
    (!inner.is_empty() && !inner.contains(',')).then_some(0)
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
    let w = word(trimmed);
    let is_decl = if w == "async" {
        word(after(trimmed, "async")) == "def"
    } else {
        w == "def" || w == "class"
    };
    is_decl.then(|| indent_of(line) / 4)
}

/// Python's body rule: skip multiline header brackets, then end at the last
/// following line more indented than the declaration.
fn python_end(lines: &[&str], start: usize) -> usize {
    let own = indent_of(lines[start]);
    let mut end = start;
    let mut bracket_depth = 0i32;
    for c in lines[start].chars() {
        match c {
            '(' | '[' | '{' => bracket_depth += 1,
            ')' | ']' | '}' => bracket_depth -= 1,
            _ => {}
        }
    }
    for (j, line) in lines.iter().enumerate().skip(start + 1) {
        if bracket_depth > 0 {
            for c in line.chars() {
                match c {
                    '(' | '[' | '{' => bracket_depth += 1,
                    ')' | ']' | '}' => bracket_depth -= 1,
                    _ => {}
                }
            }
            end = j;
            continue;
        }
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
    let raw_lines: Vec<&str> = text.lines().collect();
    let clean: Vec<String>;
    let scan_lines: Vec<&str> = if ext == "rs" {
        clean = rust_code(text);
        clean.iter().map(String::as_str).collect()
    } else {
        raw_lines.clone()
    };
    let classify: fn(&Line) -> Option<&'static str> = match ext.as_str() {
        "rs" => rust_kind,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => js_kind,
        "go" => go_kind,
        "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "cs" => c_kind,
        "sh" | "bash" | "zsh" => shell_kind,
        _ => fallback_kind,
    };
    brace_scan(&scan_lines, &raw_lines, cancel, classify)
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
