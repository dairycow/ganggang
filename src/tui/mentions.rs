//! `@path` mentions (spec §02, M7.5): at submission time, editor text
//! that mentions files carries their content to the model. The user's
//! line stays readable (mentions intact); attachments follow as
//! fenced blocks. Every limit is visible — binary, missing, and
//! cap-skipped files say so instead of silently dropping.

use std::io::Read;
use std::path::Path;

/// Total attachment budget across one submission. Generous for text,
/// small enough that a stray `@big-dir` cannot blow the context.
const ATTACHMENT_CAP: usize = 50 * 1024;

/// Bytes read per file before deciding it is text — a NUL this early is
/// the usual binary tell, and any real text file identifies itself.
const BINARY_SNIFF: usize = 8_192;

/// Expand every `@path` token in `text` (resolved against `cwd`) into
/// attachment blocks after the original text. A token mentions a file
/// when its rest contains `/` or `.` — the "looks like a path" rule,
/// which keeps `user@host` and a lone `@` literal. Trailing sentence
/// punctuation is not part of the path (`@file.txt,` mentions
/// `file.txt`): mentions sit mid-sentence, filenames ending in
/// punctuation are vanishingly rare.
pub fn expand_mentions(text: &str, cwd: &Path) -> String {
    match attachment_block(text, cwd) {
        Some(block) => [text, &block].concat(),
        None => text.to_string(),
    }
}

/// The attachment blocks for the `@path` mentions in `text` (resolved
/// against `cwd`), leading with `"\n\n--- attachments ---"`, or `None`
/// when `text` holds no mentions. What [`expand_mentions`] appends, and
/// nothing more — what a caller wants attached without repeating the
/// text itself.
pub fn attachment_block(text: &str, cwd: &Path) -> Option<String> {
    // Dedupe by the RESOLVED file, not the typed spelling: `@a.txt` and
    // `@./a.txt` are one attachment (canonicalize where possible; a path
    // that cannot be resolved still dedupes by its lexical form).
    let mut seen: Vec<std::path::PathBuf> = Vec::new();
    let mut mentioned: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let Some(path) = mention_path(token) else {
            continue;
        };
        let resolved = std::fs::canonicalize(cwd.join(path)).unwrap_or_else(|_| cwd.join(path));
        if !seen.contains(&resolved) {
            seen.push(resolved);
            mentioned.push(path.to_string());
        }
    }
    if mentioned.is_empty() {
        return None;
    }

    let mut out = String::from("\n\n--- attachments ---");
    let mut budget = ATTACHMENT_CAP;
    for path in &mentioned {
        out.push_str(&format!("\n\n--- @{path} "));
        let full = cwd.join(path);
        match read_attachment(&full, budget) {
            Attachment::Text { content, lines } => {
                budget = budget.saturating_sub(content.len());
                out.push_str(&format!(
                    "({lines} lines) ---\n{content}\n--- end @{path} ---"
                ));
            }
            Attachment::Truncated {
                content,
                lines,
                total,
            } => {
                budget = 0;
                out.push_str(&format!(
                    "({lines} of {total} lines, cap reached) ---\n{content}\n\
                     [truncated: attachment cap]\n--- end @{path} ---"
                ));
            }
            Attachment::Binary { bytes } => {
                out.push_str(&format!("— binary file, {bytes} bytes ---"));
            }
            Attachment::Missing => {
                out.push_str("— no such file ---");
            }
            Attachment::OverCap => {
                out.push_str("— skipped: attachment cap reached ---");
            }
        }
    }
    Some(out)
}

/// The path a token mentions, or `None` when it is not a mention.
fn mention_path(token: &str) -> Option<&str> {
    let rest = token.strip_prefix('@')?;
    let rest = rest.trim_end_matches([',', '.', ';', ':', '!', '?', ')', '"', '\'', '>', ']']);
    if rest.is_empty() {
        return None;
    }
    // The looks-like-a-path rule: emails (`user@host`) and bare `@` stay
    // literal; `@file.txt` and `@src/main.rs` mention.
    (rest.contains('/') || rest.contains('.')).then_some(rest)
}

/// What a mention resolved to. Truncations carry what fit, so the model
/// sees partial content plus an explicit marker — never a silent cut.
enum Attachment {
    Text {
        content: String,
        lines: usize,
    },
    Truncated {
        content: String,
        lines: usize,
        total: usize,
    },
    Binary {
        bytes: u64,
    },
    Missing,
    OverCap,
}

fn read_attachment(path: &Path, budget: usize) -> Attachment {
    let Ok(meta) = std::fs::metadata(path) else {
        return Attachment::Missing;
    };
    if !meta.is_file() {
        return Attachment::Missing;
    }
    if budget == 0 {
        return Attachment::OverCap;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return Attachment::Missing;
    };
    let mut head = vec![0u8; BINARY_SNIFF];
    let Ok(read) = file.read(&mut head) else {
        return Attachment::Missing;
    };
    head.truncate(read);
    if head.contains(&0) {
        return Attachment::Binary { bytes: meta.len() };
    }
    // Text: read up to the remaining budget (plus one byte to detect the
    // cut), landing on a char boundary.
    let mut bytes = head;
    let remaining_in_file = (meta.len() as usize).saturating_sub(bytes.len());
    let take = (budget.saturating_sub(bytes.len()) + 1).min(remaining_in_file);
    let mut rest = vec![0u8; take];
    if !rest.is_empty() {
        // `read`, not `read_exact`: a file that shrank concurrently must
        // not leak the buffer's unwritten tail into the attachment.
        let n = file.read(&mut rest).unwrap_or(0);
        rest.truncate(n);
    }
    bytes.extend_from_slice(&rest);
    // Re-check past the sniff: a NUL hiding later in what we actually
    // read (the sniff covers only the first BINARY_SNIFF bytes) makes
    // this a binary file, not a text attachment with NULs in it.
    if bytes.contains(&0) {
        return Attachment::Binary { bytes: meta.len() };
    }
    let fits = bytes.len() <= budget;
    if !fits {
        bytes.truncate(budget);
        while bytes.last().is_some_and(|b| b & 0xC0 == 0x80) {
            bytes.pop(); // drop a partial UTF-8 tail
        }
        bytes.pop(); // drop the char the tail belonged to
    }
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let lines = content.lines().count();
    if fits {
        Attachment::Text { content, lines }
    } else {
        Attachment::Truncated {
            content,
            lines,
            // Counted only when truncating — and streamed, so a huge
            // log is never held in memory just to say how big it is.
            total: count_lines(path),
        }
    }
}

/// Total line count for the `(N of M lines)` truncation marker.
fn count_lines(path: &Path) -> usize {
    std::fs::File::open(path)
        .map(|f| {
            use std::io::BufRead as _;
            std::io::BufReader::new(f).split(b'\n').count().max(1)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gg-mentions-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn token_rule_keeps_emails_and_lone_ats_literal() {
        assert_eq!(mention_path("@src/main.rs"), Some("src/main.rs"));
        assert_eq!(mention_path("@file.txt"), Some("file.txt"));
        assert_eq!(mention_path("@host"), None, "email style stays literal");
        assert_eq!(mention_path("@"), None);
        assert_eq!(mention_path("plain"), None);
    }

    /// Mentions sit mid-sentence: trailing punctuation is sentence
    /// structure, not filename.
    #[test]
    fn trailing_punctuation_is_not_part_of_the_path() {
        assert_eq!(mention_path("@file.txt?"), Some("file.txt"));
        assert_eq!(mention_path("@file.txt."), Some("file.txt"));
        assert_eq!(mention_path("@src/main.rs),"), Some("src/main.rs"));
        assert_eq!(mention_path("@file.txt?!"), Some("file.txt"));
        assert_eq!(mention_path("@what?"), None, "still needs a path shape");
    }

    #[test]
    fn a_mention_attaches_the_file_and_keeps_the_line() {
        let d = dir("basic");
        fs::write(d.join("notes.txt"), "line one\nline two\n").unwrap();
        let out = expand_mentions("look at @notes.txt please", &d);
        assert!(out.starts_with("look at @notes.txt please\n\n--- attachments ---"));
        assert!(out.contains("--- @notes.txt (2 lines) ---\nline one\nline two\n"));
        assert!(out.contains("--- end @notes.txt ---"));
    }

    #[test]
    fn repeated_mentions_attach_once_in_first_order() {
        let d = dir("dedupe");
        fs::write(d.join("a.txt"), "A").unwrap();
        fs::write(d.join("b.txt"), "B").unwrap();
        let out = expand_mentions("@b.txt then @a.txt and again @b.txt", &d);
        assert_eq!(out.matches("--- @b.txt").count(), 1);
        assert_eq!(out.matches("--- @a.txt").count(), 1);
        let b_at = out.find("--- @b.txt").unwrap();
        let a_at = out.find("--- @a.txt").unwrap();
        assert!(b_at < a_at, "first-occurrence order");
    }

    /// Spellings of the SAME file dedupe: `@a.txt` and `@./a.txt` are
    /// one attachment (resolved-path identity, not string identity).
    #[test]
    fn equivalent_path_spellings_attach_once() {
        let d = dir("dedupe-paths");
        fs::write(d.join("a.txt"), "A").unwrap();
        let out = expand_mentions("see @a.txt and @./a.txt", &d);
        assert_eq!(
            out.matches("--- @a.txt").count(),
            1,
            "first spelling heads it"
        );
        assert_eq!(out.matches("--- end @a.txt").count(), 1);
        assert_eq!(
            out.matches("A").count(),
            1,
            "the content rides along exactly once"
        );
    }

    /// A NUL past the 8 KiB sniff still makes the file binary — the
    /// re-check covers everything actually read, so no NUL rides into a
    /// "text" attachment.
    #[test]
    fn a_nul_past_the_sniff_still_reads_as_binary() {
        let d = dir("late-nul");
        let mut blob = vec![b'a'; 9_000];
        blob.push(0);
        blob.extend_from_slice(b"tail");
        fs::write(d.join("log.txt"), &blob).unwrap();
        let out = expand_mentions("@log.txt", &d);
        assert!(
            out.contains("@log.txt — binary file, 9005 bytes ---"),
            "{out}"
        );
    }

    #[test]
    fn missing_files_say_so_instead_of_attaching() {
        let d = dir("missing");
        let out = expand_mentions("read @nope.txt", &d);
        assert!(out.contains("--- @nope.txt — no such file ---"));
        assert!(out.starts_with("read @nope.txt"));
    }

    #[test]
    fn binary_files_report_their_size() {
        let d = dir("binary");
        fs::write(d.join("blob.bin"), [0u8, 1, 0, 2, 3]).unwrap();
        let out = expand_mentions("check @blob.bin", &d);
        assert!(out.contains("--- @blob.bin — binary file, 5 bytes ---"));
    }

    #[test]
    fn no_mentions_returns_the_text_unchanged() {
        let d = dir("none");
        assert_eq!(
            expand_mentions("email me at bob@host", &d),
            "email me at bob@host"
        );
    }

    #[test]
    fn the_cap_truncates_visibly_and_skips_the_rest() {
        let d = dir("cap");
        // Two files that together exceed the cap; the first alone fits
        // only partially at this small size — so shrink to a controlled
        // setup: one 60KB file (partial) then one more (skipped).
        let big = "x".repeat(60 * 1024);
        fs::write(d.join("big.txt"), &big).unwrap();
        fs::write(d.join("more.txt"), "more").unwrap();
        let out = expand_mentions("@big.txt @more.txt", &d);
        assert!(out.contains("of 1 lines, cap reached)"), "{}", &out[..120]);
        assert!(out.contains("[truncated: attachment cap]"));
        assert!(out.contains("--- @more.txt — skipped: attachment cap reached ---"));
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        let d = dir("relative");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub").join("f.txt"), "deep").unwrap();
        let out = expand_mentions("see @sub/f.txt", &d);
        assert!(out.contains("--- @sub/f.txt (1 lines) ---\ndeep"));
    }
}
