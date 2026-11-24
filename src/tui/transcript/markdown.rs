//! Minimal markdown rendering for assistant text (spec §15, M7.6):
//! fenced code blocks, bold spans, and list markers — nothing else,
//! hand-rolled. Line-oriented on purpose: the transcript re-renders each
//! frame from the block's full accumulated text, so streaming needs no
//! incremental bookkeeping and an unterminated fence simply renders as
//! code until the stream closes it.

use super::render::{LineKind, Mark, RenderedLine};

/// Split assistant text into rendered lines. A line whose trimmed form
/// starts with ``` toggles code mode (the fence line itself carries no
/// content; an info string like ```rust is ignored). `**` pairs bold
/// their span; an unpaired trailing marker renders plain. `- `/`* `
/// bullets become `• ` (indent preserved); ordered markers stay typed.
pub(super) fn lines(text: &str) -> Vec<RenderedLine> {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(RenderedLine::new(line, LineKind::Code));
            continue;
        }
        let displayed = bulletize(line);
        let (plain, bold) = strip_bold(&displayed);
        let marks = bold
            .into_iter()
            .map(|(start, end)| (start, end, Mark::Bold))
            .collect();
        out.push(RenderedLine::new(plain, LineKind::Assistant).with_marks(marks));
    }
    out
}

/// `- ` / `* ` after leading whitespace becomes `• `. A `* ` marker must
/// carry its space to stay distinct from a bold span; `**…` lines are
/// therefore never bullets.
fn bulletize(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    let trimmed = &line[indent..];
    let marker = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "));
    match marker {
        Some(rest) => format!("{}• {rest}", &line[..indent]),
        None => line.to_string(),
    }
}

/// Remove `**` markers, returning the plain text and the char ranges
/// (into that text) to render bold. Splitting on the marker makes pairs
/// fall on odd segments; an odd marker count leaves the final segment
/// unpaired, which renders plain — no mid-stream flicker.
fn strip_bold(line: &str) -> (String, Vec<(usize, usize)>) {
    if !line.contains("**") {
        return (line.to_string(), Vec::new());
    }
    let segments: Vec<&str> = line.split("**").collect();
    let unpaired_tail = segments.len().is_multiple_of(2);
    let mut text = String::new();
    let mut bold = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        if i % 2 == 1 && !(unpaired_tail && i + 1 == segments.len()) && !segment.is_empty() {
            let start = text.chars().count();
            text.push_str(segment);
            bold.push((start, text.chars().count()));
        } else {
            text.push_str(segment);
        }
    }
    (text, bold)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (text, kind, marks) per rendered line — plain data, comparable.
    type Flat = Vec<(String, LineKind, Vec<(usize, usize, Mark)>)>;

    fn rendered(text: &str) -> Flat {
        lines(text)
            .into_iter()
            .map(|l| (l.text, l.kind, l.marks))
            .collect()
    }

    /// `(start, end)` as a bold mark, the shape the tests read.
    fn b(start: usize, end: usize) -> (usize, usize, Mark) {
        (start, end, Mark::Bold)
    }

    #[test]
    fn fenced_code_renders_as_code_and_fence_lines_drop() {
        let out = rendered("before\n```rust\nfn a() {}\n**not bold**\n```\nafter");
        assert_eq!(
            out,
            vec![
                ("before".into(), LineKind::Assistant, vec![]),
                ("fn a() {}".into(), LineKind::Code, vec![]),
                // Inside a fence, markers are literal — no stripping.
                ("**not bold**".into(), LineKind::Code, vec![]),
                ("after".into(), LineKind::Assistant, vec![]),
            ]
        );
    }

    #[test]
    fn unterminated_fence_renders_the_rest_as_code() {
        let out = rendered("intro\n```\ncode so far");
        assert_eq!(out[0].0, "intro");
        assert_eq!(out[1], ("code so far".into(), LineKind::Code, vec![]));
    }

    #[test]
    fn bold_pairs_mark_ranges_and_unpaired_tails_stay_plain() {
        let (text, bold) = strip_bold("**Note:** plain **tail**");
        assert_eq!(text, "Note: plain tail");
        assert_eq!(bold, vec![(0, 5), (12, 16)]);

        let (text, bold) = strip_bold("a**unpaired");
        assert_eq!(text, "aunpaired");
        assert!(bold.is_empty(), "no closing marker: no bold");

        let (text, bold) = strip_bold("no markers");
        assert_eq!(text, "no markers");
        assert!(bold.is_empty());

        let (text, bold) = strip_bold("****");
        assert_eq!(text, "");
        assert!(bold.is_empty(), "empty bold span records nothing");
    }

    #[test]
    fn bullets_become_dots_with_indent_and_ordered_stay_typed() {
        assert_eq!(bulletize("- one"), "• one");
        assert_eq!(bulletize("  * two"), "  • two");
        // A bold span is not a bullet; an ordered list is already fine.
        assert_eq!(bulletize("**bold** line"), "**bold** line");
        assert_eq!(bulletize("1. three"), "1. three");
        assert_eq!(bulletize("no list"), "no list");
    }

    #[test]
    fn a_full_assistant_block_renders_through_the_pass() {
        let out = rendered("- item\n  - nested\n1. first\n**key:** value");
        assert_eq!(
            out,
            vec![
                ("• item".into(), LineKind::Assistant, vec![]),
                ("  • nested".into(), LineKind::Assistant, vec![]),
                ("1. first".into(), LineKind::Assistant, vec![]),
                ("key: value".into(), LineKind::Assistant, vec![b(0, 4)]),
            ]
        );
    }

    #[test]
    fn an_extremely_long_fence_renders_without_panicking() {
        let fence = format!("```\n{}", "line\n".repeat(10_000));
        let out = lines(&fence);
        // 10_000 "line" rows plus the empty tail after the last newline.
        assert_eq!(out.len(), 10_001);
        assert!(out.iter().all(|l| l.kind == LineKind::Code));
    }
}
