//! Brace languages: one shared scanner, one classifier per family. `brace_scan`
//! walks the lines, asks its classifier what item each line opens, and spans
//! that item to its closing brace or `;`.

use super::super::Cancel;
use super::lang::{Entry, Found, after, stopped, word};

/// The line where the item opened at `start` ends: the line whose `;` or
/// closing brace returns the brace depth to the item's own level.
fn item_end(lines: &[&str], start: usize) -> usize {
    let mut depth: i32 = 0;
    let mut opened = false;
    for (j, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                ';' if depth == 0 => return j,
                _ => continue,
            }
            opened = true;
        }
        if opened && depth <= 0 {
            return j;
        }
    }
    lines.len().saturating_sub(1)
}

/// One line as a brace scanner sees it.
pub(super) struct Line<'a> {
    text: &'a str,
    depth: usize,
    parent: Option<&'static str>,
}

/// Walk `lines`, asking `classify` what item each line opens. An entry spans
/// to `item_end`; brace depth and the enclosing item follow every line.
pub(super) fn brace_scan(
    lines: &[&str],
    cancel: &Cancel,
    classify: impl Fn(&Line) -> Option<&'static str>,
) -> Found {
    let mut entries = Vec::new();
    let mut depth: usize = 0;
    let mut parents: Vec<&'static str> = Vec::new();
    for (i, text) in lines.iter().enumerate() {
        if stopped(cancel, i) {
            return None;
        }
        let kind = classify(&Line {
            text,
            depth,
            parent: parents.last().copied(),
        });
        if kind.is_some() {
            entries.push(Entry {
                start: i + 1,
                end: item_end(lines, i) + 1,
                depth,
                sig: text.trim().to_string(),
            });
        }
        for ch in text.chars() {
            if ch == '{' {
                depth += 1;
                parents.push(kind.unwrap_or("block"));
            } else if ch == '}' {
                depth = depth.saturating_sub(1);
                parents.pop();
            }
        }
    }
    Some(entries)
}

/// The Rust scanner's lexical state.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    Code,
    Line,
    Block,
    Str,
    Char,
}

/// `text` with comments dropped and string and char bodies blanked, so
/// braces and keywords inside them cannot match.
pub(super) fn rust_code(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut state = Lex::Code;
    let mut blocks = 0usize;
    for line in text.lines() {
        let mut clean = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        let mut escaped = false;
        while let Some(c) = chars.next() {
            let next = *chars.peek().unwrap_or(&'\0');
            match state {
                Lex::Line => {}
                Lex::Block if (c, next) == ('*', '/') => {
                    chars.next();
                    blocks -= 1;
                    state = if blocks == 0 { Lex::Code } else { state };
                }
                Lex::Block if (c, next) == ('/', '*') => {
                    chars.next();
                    blocks += 1;
                }
                Lex::Block => {}
                Lex::Str | Lex::Char if escaped => escaped = false,
                Lex::Str | Lex::Char if c == '\\' => escaped = true,
                Lex::Str if c == '"' => state = Lex::Code,
                Lex::Char if c == '\'' => state = Lex::Code,
                Lex::Str | Lex::Char => {}
                Lex::Code if (c, next) == ('/', '/') => state = Lex::Line,
                Lex::Code if (c, next) == ('/', '*') => {
                    chars.next();
                    blocks = 1;
                    state = Lex::Block;
                }
                Lex::Code if c == '"' => state = Lex::Str,
                // A `'` opens a char literal when its body is escaped or one
                // character long; otherwise it starts a lifetime.
                Lex::Code if c == '\'' && (next == '\\' || chars.clone().nth(1) == Some('\'')) => {
                    state = Lex::Char
                }
                Lex::Code => clean.push(c),
            }
        }
        if state == Lex::Line {
            state = Lex::Code;
        }
        out.push(clean);
    }
    out
}

/// Rust item keywords a top-level line may open with.
const RUST_ITEMS: [&str; 8] = [
    "fn", "struct", "enum", "trait", "impl", "mod", "type", "static",
];

/// The item keyword a cleaned Rust line opens, past its modifiers.
fn rust_keyword(line: &str) -> Option<&'static str> {
    let mut rest = line.trim_start();
    loop {
        let w = word(rest);
        match w {
            "pub" => {
                rest = after(rest, w);
                if let Some(tail) = rest.strip_prefix('(') {
                    rest = tail.split_once(')').map_or("", |(_, r)| r).trim_start();
                }
            }
            "default" | "async" | "unsafe" | "extern" => rest = after(rest, w),
            // `const fn` is a modifier; `const NAME` is the item itself.
            "const" => {
                let tail = after(rest, w);
                if matches!(word(tail), "fn" | "unsafe" | "extern" | "mut") {
                    rest = tail;
                } else {
                    return Some("const");
                }
            }
            "macro_rules" => return Some("macro_rules!"),
            // Any other word is an item keyword, or the line defines nothing.
            _ => return RUST_ITEMS.iter().copied().find(|k| *k == w),
        }
    }
}

/// Top level names every kind of item; inside an impl, trait or mod only the
/// methods, so a function body stays one entry rather than a list.
pub(super) fn rust_kind(line: &Line) -> Option<&'static str> {
    if line.depth > 0 {
        if !matches!(line.parent, Some("impl" | "trait" | "mod")) {
            return None;
        }
        let mut rest = line.text.trim_start();
        loop {
            let w = word(rest);
            if matches!(
                w,
                "pub" | "default" | "async" | "unsafe" | "const" | "extern"
            ) {
                rest = after(rest, w);
                continue;
            }
            break (w == "fn").then_some("fn");
        }
    } else {
        rust_keyword(line.text)
    }
}

/// Declaration keywords a JS or TS line may open with.
const JS_DECLS: [&str; 4] = ["function", "class", "interface", "enum"];

/// The declaration keyword of a JS or TS line, past its modifiers. A `type`
/// followed by `=` is a variable, not TypeScript's type alias.
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
        let alias = w == "type" && !word(after(rest, w)).is_empty();
        if alias {
            return Some("type");
        }
        return JS_DECLS.iter().copied().find(|k| *k == w);
    }
}

pub(super) fn js_kind(line: &Line) -> Option<&'static str> {
    let trimmed = line.text.trim();
    if let Some(kind) = js_keyword(line.text) {
        return Some(kind);
    }
    let bound = ["const ", "let ", "var "]
        .iter()
        .any(|p| trimmed.starts_with(p));
    if bound && (trimmed.contains("=>") || trimmed.contains("= function")) {
        return Some("function");
    }
    // A method inside a class body: one brace deep, a head that opens a block.
    let method = line.depth == 1
        && line.parent == Some("class")
        && trimmed.contains('(')
        && trimmed.contains('{')
        && !word(trimmed).is_empty();
    method.then_some("function")
}

/// Top-level Go declarations worth an entry.
const GO_DECLS: [&str; 2] = ["func", "type"];

pub(super) fn go_kind(line: &Line) -> Option<&'static str> {
    if line.depth > 0 {
        return None;
    }
    let w = word(line.text.trim_start());
    GO_DECLS.iter().copied().find(|k| *k == w)
}

/// Words that open a brace but define nothing.
const NOT_A_DEF: &[&str] = &[
    "if", "for", "while", "switch", "catch", "else", "return", "do",
];

/// Type keywords a C-family line may open with.
const C_DECLS: [&str; 6] = ["struct", "enum", "union", "class", "interface", "namespace"];

pub(super) fn c_kind(line: &Line) -> Option<&'static str> {
    let w = word(line.text.trim_start());
    if let Some(kind) = C_DECLS.iter().copied().find(|k| *k == w) {
        return Some(kind);
    }
    let trimmed = line.text.trim();
    if line.depth > 0 || !trimmed.ends_with('{') || !trimmed.contains('(') {
        return None;
    }
    (!w.is_empty() && !NOT_A_DEF.contains(&w)).then_some("fn")
}

pub(super) fn shell_kind(line: &Line) -> Option<&'static str> {
    let trimmed = line.text.trim();
    if line.depth > 0 || trimmed.starts_with('#') {
        return None;
    }
    let declared =
        trimmed.starts_with("function ") || trimmed.ends_with("() {") || trimmed.ends_with("(){");
    declared.then_some("function")
}

/// Unknown languages: top-level definitions and `# ` headings, which cover
/// most Kotlin, Ruby, Lua and plain-text files well enough to jump into.
pub(super) fn fallback_kind(line: &Line) -> Option<&'static str> {
    let trimmed = line.text.trim_start();
    if trimmed.starts_with('#') {
        return (line.depth == 0 && trimmed.starts_with("# ")).then_some("heading");
    }
    if line.depth > 0 || !line.text.starts_with(|c: char| c.is_alphabetic()) {
        return None;
    }
    let w = word(trimmed);
    matches!(
        w,
        "fn" | "def" | "class" | "func" | "function" | "struct" | "enum" | "interface"
    )
    .then_some("fn")
}
