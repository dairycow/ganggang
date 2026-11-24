//! Rust lexical cleaner and item classifier for `outline`.
//!
//! Comments and string/char/raw-string bodies are blanked before brace
//! scanning so braces and keywords inside them cannot skew depth or match.

use super::brace::Line;
use super::lang::{after, word};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    Code,
    Line,
    Block,
    Str,
    Char,
    RawStr(usize),
}

fn char_like(next: char, then: char) -> bool {
    next == '\\' || then == '\''
}

/// Count `#`s if `c == 'r'` (or `b`/`c` followed by `r`) opens a raw string
/// (`r"..."`, `r#"..."#`, `br#"..."#`, `cr#"..."#`). Returns `(hashes, skip_chars)`.
fn raw_string_start(
    c: char,
    chars: &std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<(usize, usize)> {
    let mut look = chars.clone();
    let mut prefix = 0usize;
    if c == 'r' {
        // already at 'r'
    } else if (c == 'b' || c == 'c') && look.next() == Some('r') {
        prefix = 1;
    } else {
        return None;
    }
    let mut hashes = 0usize;
    while look.peek() == Some(&'#') {
        look.next();
        hashes += 1;
    }
    if look.next() == Some('"') {
        Some((hashes, prefix + hashes + 1))
    } else {
        None
    }
}

/// `text` with comments dropped and string/char/raw-string bodies blanked.
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
                    if blocks == 0 {
                        state = Lex::Code;
                        clean.push(' ');
                    }
                }
                Lex::Block if (c, next) == ('/', '*') => {
                    chars.next();
                    blocks += 1;
                }
                Lex::Block => {}
                Lex::RawStr(hashes) if c == '"' => {
                    let mut look = chars.clone();
                    let mut matched = 0usize;
                    while matched < hashes && look.next() == Some('#') {
                        matched += 1;
                    }
                    if matched == hashes {
                        for _ in 0..hashes {
                            chars.next();
                        }
                        state = Lex::Code;
                    }
                }
                Lex::RawStr(_) => {}
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
                Lex::Code if c == '\'' && char_like(next, chars.clone().nth(1).unwrap_or('\0')) => {
                    state = Lex::Char;
                }
                Lex::Code => {
                    if let Some((hashes, skip)) = raw_string_start(c, &chars) {
                        for _ in 0..skip {
                            chars.next();
                        }
                        state = Lex::RawStr(hashes);
                    } else {
                        clean.push(c);
                    }
                }
            }
        }
        if matches!(state, Lex::Line | Lex::Char) {
            state = Lex::Code;
        }
        out.push(clean);
    }
    out
}

const RUST_ITEMS: [&str; 8] = [
    "fn", "struct", "enum", "trait", "impl", "mod", "type", "static",
];

/// The item keyword a cleaned Rust line opens, past attributes and modifiers.
pub(super) fn rust_keyword(line: &str) -> Option<&'static str> {
    let mut rest = line.trim_start();
    while rest.starts_with("#[") {
        let (_, after_attr) = rest.split_once(']')?;
        rest = after_attr.trim_start();
    }
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
            "const" => {
                let tail = after(rest, w);
                if matches!(word(tail), "fn" | "unsafe" | "extern" | "mut") {
                    rest = tail;
                } else {
                    return Some("const");
                }
            }
            "fn" | "struct" | "enum" | "trait" | "impl" | "mod" | "type" | "static" => {
                return RUST_ITEMS.iter().copied().find(|k| *k == w);
            }
            "macro_rules" => return Some("macro_rules!"),
            _ => return None,
        }
    }
}

/// Top level and inline `mod`s name every item; `impl`/`trait` name methods.
pub(super) fn rust_kind(line: &Line) -> Option<&'static str> {
    if line.depth == 0 || line.parent == Some("mod") {
        rust_keyword(line.text)
    } else if matches!(line.parent, Some("impl" | "trait")) {
        rust_keyword(line.text).filter(|k| *k == "fn")
    } else {
        None
    }
}
