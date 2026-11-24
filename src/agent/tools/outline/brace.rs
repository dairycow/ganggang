//! Brace-language scanners for `outline`: Rust, JS/TS, Go, C-family,
//! Shell, and a generic keyword fallback.

use super::super::Cancel;
use super::lang::{Entry, Found, after, stopped, word};

/// One line as a brace scanner sees it.
pub(super) struct Line<'a> {
    pub text: &'a str,
    pub depth: usize,
    pub parent: Option<&'static str>,
}

/// True when `lines[start]` has no `{` and the next non-blank line does not
/// start with `{` (e.g. Go `type Alias string` or JS `type Foo = Bar`).
fn is_single_line_decl(lines: &[&str], start: usize) -> bool {
    if lines[start].contains('{') || lines[start].contains(';') {
        return false;
    }
    for line in lines.iter().skip(start + 1) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        return !t.starts_with('{');
    }
    true
}

/// The line where the item opened at `start` ends: the line whose top-level
/// `;` or closing brace returns the brace depth to the item's own level.
fn item_end(lines: &[&str], start: usize, cancel: &Cancel) -> Option<usize> {
    if is_single_line_decl(lines, start) {
        return Some(start);
    }
    let mut brace: i32 = 0;
    let mut paren: i32 = 0;
    let mut bracket: i32 = 0;
    let mut opened = false;
    for (j, line) in lines.iter().enumerate().skip(start) {
        if stopped(cancel, j) {
            return None;
        }
        for ch in line.chars() {
            match ch {
                '{' => {
                    brace += 1;
                    opened = true;
                }
                '}' => {
                    brace -= 1;
                    opened = true;
                }
                '(' => paren += 1,
                ')' => paren = paren.saturating_sub(1),
                '[' => bracket += 1,
                ']' => bracket = bracket.saturating_sub(1),
                ';' if brace == 0 && paren == 0 && bracket == 0 => return Some(j),
                _ => {}
            }
        }
        if opened && brace <= 0 {
            return Some(j);
        }
    }
    Some(lines.len().saturating_sub(1))
}

/// Walk `scan_lines`, asking `classify` what item each line opens, and
/// rendering signatures from `raw_lines`.
pub(super) fn brace_scan(
    scan_lines: &[&str],
    raw_lines: &[&str],
    cancel: &Cancel,
    classify: impl Fn(&Line) -> Option<&'static str>,
) -> Found {
    let mut entries = Vec::new();
    let mut depth: usize = 0;
    let mut parents: Vec<&'static str> = Vec::new();
    let mut pending_kind: Option<&'static str> = None;
    let mut paren: i32 = 0;
    let mut bracket: i32 = 0;
    for (i, text) in scan_lines.iter().enumerate() {
        if stopped(cancel, i) {
            return None;
        }
        let kind = classify(&Line {
            text,
            depth,
            parent: parents.last().copied(),
        });
        if kind.is_some() {
            pending_kind = kind;
            let end = item_end(scan_lines, i, cancel)?;
            entries.push(Entry {
                start: i + 1,
                end: end + 1,
                depth,
                sig: raw_lines.get(i).copied().unwrap_or(text).trim().to_string(),
            });
        }
        for ch in text.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    parents.push(pending_kind.take().unwrap_or("block"));
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    parents.pop();
                }
                '(' => paren += 1,
                ')' => paren = paren.saturating_sub(1),
                '[' => bracket += 1,
                ']' => bracket = bracket.saturating_sub(1),
                ';' if paren == 0 && bracket == 0 => pending_kind = None,
                _ => {}
            }
        }
    }
    Some(entries)
}

const JS_DECLS: [&str; 4] = ["function", "class", "interface", "enum"];

fn js_keyword(line: &str) -> Option<&'static str> {
    let mut rest = line.trim_start();
    loop {
        let w = word(rest);
        if matches!(
            w,
            "export" | "default" | "declare" | "async" | "abstract" | "public" | "static"
        ) {
            rest = after(rest, w);
            continue;
        }
        if w == "type" && !word(after(rest, w)).is_empty() {
            return Some("type");
        }
        return JS_DECLS.iter().copied().find(|k| *k == w);
    }
}

/// JS/TS declarations, top-level arrow/function bindings, and class methods.
pub(super) fn js_kind(line: &Line) -> Option<&'static str> {
    if let Some(kind) = js_keyword(line.text) {
        return Some(kind);
    }
    let mut trimmed = line.text.trim();
    for prefix in ["export ", "default "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            trimmed = rest.trim_start();
        }
    }
    if line.depth == 0
        && ["const ", "let ", "var "]
            .iter()
            .any(|p| trimmed.starts_with(p))
        && let Some((_, rhs)) = trimmed.split_once('=')
    {
        let rhs = rhs.trim_start();
        let is_fn = rhs.starts_with("function")
            || rhs.starts_with("async function")
            || (rhs.contains("=>") && !rhs.split("=>").next().unwrap_or("").contains('.'));
        if is_fn {
            return Some("function");
        }
    }
    let method = line.depth == 1
        && line.parent == Some("class")
        && trimmed.contains('(')
        && (trimmed.contains('{') || trimmed.ends_with(')'))
        && !word(trimmed).is_empty();
    method.then_some("function")
}

const GO_DECLS: [&str; 2] = ["func", "type"];

/// Top-level Go `func` and `type` declarations.
pub(super) fn go_kind(line: &Line) -> Option<&'static str> {
    if line.depth > 0 {
        return None;
    }
    let w = word(line.text.trim_start());
    GO_DECLS.iter().copied().find(|k| *k == w)
}

const NOT_A_DEF: &[&str] = &[
    "if", "for", "while", "switch", "catch", "else", "return", "do",
];
const C_DECLS: [&str; 6] = ["struct", "enum", "union", "class", "interface", "namespace"];

/// C, C++, Java, and C# types, namespaces, and functions/methods.
pub(super) fn c_kind(line: &Line) -> Option<&'static str> {
    let mut rest = line.text.trim_start();
    loop {
        let w = word(rest);
        if matches!(
            w,
            "public"
                | "private"
                | "protected"
                | "internal"
                | "static"
                | "final"
                | "abstract"
                | "sealed"
                | "partial"
                | "virtual"
                | "override"
                | "async"
                | "extern"
                | "inline"
                | "constexpr"
                | "typedef"
        ) {
            rest = after(rest, w);
            continue;
        }
        break;
    }
    let w = word(rest);
    let in_type_scope = line.depth == 0
        || matches!(
            line.parent,
            Some("namespace" | "class" | "interface" | "struct")
        );
    if in_type_scope
        && let Some(kind) = C_DECLS.iter().copied().find(|k| *k == w)
        && !rest.trim_end().ends_with(';')
        && !rest.contains('=')
        && !rest.contains('*')
    {
        return Some(kind);
    }
    let in_fn_scope =
        line.depth == 0 || matches!(line.parent, Some("namespace" | "class" | "interface"));
    let trimmed = line.text.trim();
    if !in_fn_scope || !trimmed.ends_with('{') || !trimmed.contains('(') {
        return None;
    }
    (!w.is_empty() && !NOT_A_DEF.contains(&w)).then_some("fn")
}

/// Top-level shell function declarations.
pub(super) fn shell_kind(line: &Line) -> Option<&'static str> {
    let trimmed = line.text.trim();
    if line.depth > 0 || trimmed.starts_with('#') {
        return None;
    }
    let declared =
        trimmed.starts_with("function ") || trimmed.ends_with("() {") || trimmed.ends_with("(){");
    declared.then_some("function")
}

/// Unknown languages: top-level definitions.
pub(super) fn fallback_kind(line: &Line) -> Option<&'static str> {
    let trimmed = line.text.trim_start();
    if line.depth > 0 || !line.text.starts_with(|c: char| c.is_alphabetic()) {
        return None;
    }
    matches!(
        word(trimmed),
        "fn" | "fun" | "def" | "class" | "func" | "function" | "struct" | "enum" | "interface"
    )
    .then_some("fn")
}
