//! Tests for the markdown pass: fences, bold, bullets, tables.

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
    assert_eq!(text, "a**unpaired");
    assert!(bold.is_empty(), "no closing marker: no bold");

    let (text, bold) = strip_bold("**/*.rs");
    assert_eq!(text, "**/*.rs");
    assert!(bold.is_empty());

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
fn a_header_and_delimiter_become_table_rows_with_alignment() {
    let out =
        lines("intro\n| name | kind |\n|:-----|-----:|\n| gg | cli |\n| table | tui |\nafter");
    assert_eq!(out.len(), 5);
    assert_eq!(out[0].kind, LineKind::Assistant);
    assert_eq!(
        out[1].table,
        Some(TableRow {
            cells: vec!["name".into(), "kind".into()],
            aligns: vec![Align::Left, Align::Right],
            header: true,
        })
    );
    assert_eq!(
        out[2].table,
        Some(TableRow {
            cells: vec!["gg".into(), "cli".into()],
            aligns: vec![Align::Left, Align::Right],
            header: false,
        })
    );
    assert_eq!(
        out[3].table,
        Some(TableRow {
            cells: vec!["table".into(), "tui".into()],
            aligns: vec![Align::Left, Align::Right],
            header: false,
        })
    );
    assert_eq!(out[4].text, "after");
    assert!(out[1..4].iter().all(|line| line.kind == LineKind::Table));
    assert!(
        out[1..4].iter().all(|line| line.text.is_empty()),
        "table text is laid out at draw time"
    );
}

#[test]
fn all_three_delimiter_alignments_parse() {
    let out = lines("| a | b | c |\n|:--|--:|:--:|\n| 1 | 2 | 3 |");
    let row = out[0].table.as_ref().expect("a table row");
    assert_eq!(row.aligns, vec![Align::Left, Align::Right, Align::Center]);
}

#[test]
fn a_table_ends_at_a_blank_line_or_a_line_without_pipes() {
    let out = lines("| a | b |\n|---|---|\n| 1 | 2 |\n\nnext");
    assert_eq!(out[0].kind, LineKind::Table);
    assert_eq!(out[1].kind, LineKind::Table);
    assert_eq!(out[2].text, "");
    assert_eq!(out[3].text, "next");
}

#[test]
fn a_partial_or_malformed_table_stays_literal() {
    // Streaming: the delimiter has not arrived yet.
    let out = rendered("| a | b |");
    assert_eq!(out, vec![("| a | b |".into(), LineKind::Assistant, vec![])]);

    // Column counts disagree, so it is not a table.
    let out = rendered("| a | b |\n|---|\n| 1 | 2 |");
    assert!(out.iter().all(|(_, kind, _)| *kind == LineKind::Assistant));

    // A delimiter cell must be dashes and colons only.
    let out = rendered("| a | b |\n|---| nope |\n| 1 | 2 |");
    assert!(out.iter().all(|(_, kind, _)| *kind == LineKind::Assistant));
}

#[test]
fn table_rows_are_padded_and_cut_to_the_delimiter_columns() {
    let out = lines("| a | b |\n|---|---|\n| 1 |\n| 2 | 3 | 4 |");
    assert_eq!(out[1].table.as_ref().unwrap().cells, vec!["1", ""]);
    assert_eq!(out[2].table.as_ref().unwrap().cells, vec!["2", "3"]);
}

#[test]
fn emphasis_markers_are_stripped_from_table_cells() {
    let out = lines("| **name** | kind |\n|---|---|\n| **gg** | cli |");
    assert_eq!(out[0].table.as_ref().unwrap().cells, vec!["name", "kind"]);
    assert_eq!(out[1].table.as_ref().unwrap().cells, vec!["gg", "cli"]);
}

#[test]
fn a_table_inside_a_fence_is_code() {
    let out = lines("```\n| a | b |\n|---|---|\n```");
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|line| line.kind == LineKind::Code));
}

#[test]
fn an_extremely_long_fence_renders_without_panicking() {
    let fence = format!("```\n{}", "line\n".repeat(10_000));
    let out = lines(&fence);
    // 10_000 "line" rows plus the empty tail after the last newline.
    assert_eq!(out.len(), 10_001);
    assert!(out.iter().all(|l| l.kind == LineKind::Code));
}
