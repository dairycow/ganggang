//! One rendered transcript line → what the terminal shows: wrapped rows
//! of an exact width, and each row's styled spans.
//!
//! Split from the view so the two rules that are easy to get wrong and
//! easy to test — a mark must survive a row boundary, a code row must
//! reach the row's full width to read as a band — sit in one file.

use ratatui::style::Style;
use ratatui::text::{Line as UiLine, Span};

use super::theme::Palette;
use super::transcript::{LineKind, Mark, RenderedLine};

/// Split one rendered line into rows of at most `width` chars. Char-wise,
/// so a multibyte character is never split; wide glyphs (CJK) may render a
/// row slightly long — the terminal clips the excess, nothing panics, and
/// the row count stays exact for scrolling. Marks are clipped to their
/// row, so a span split across rows styles both pieces.
pub fn wrap_line(line: &RenderedLine, width: usize) -> Vec<RenderedLine> {
    wrap_chars(&line.text, width)
        .into_iter()
        .map(|(text, row_start)| finish_row(text, line, row_start))
        .collect()
}

/// Split a string into row-length pieces at a char boundary, returning
/// each piece with the char offset it starts at. Used for the transcript
/// (where a row is a real row) and for a composer line wider than its box
/// (where the pieces are the window's columns).
pub fn wrap_chars(text: &str, width: usize) -> Vec<(String, usize)> {
    if width == 0 {
        return vec![(text.to_string(), 0)];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    // Char offset of `current`'s first char within the full text — where
    // the row under construction starts.
    let mut row_start = 0usize;
    for ch in text.chars() {
        if count == width {
            rows.push((std::mem::take(&mut current), row_start));
            row_start += width;
            count = 0;
        }
        current.push(ch);
        count += 1;
    }
    rows.push((current, row_start));
    rows
}

/// Materialize one wrapped row with its share of the line's marks
/// (shifted to row-local char offsets; empty ranges drop).
fn finish_row(text: String, line: &RenderedLine, row_start: usize) -> RenderedLine {
    let row_end = row_start + text.chars().count();
    let mut marks = Vec::new();
    for (start, end, mark) in &line.marks {
        let lo = (*start).clamp(row_start, row_end);
        let hi = (*end).clamp(row_start, row_end);
        if lo < hi {
            marks.push((lo - row_start, hi - row_start, *mark));
        }
    }
    RenderedLine::new(text, line.kind).with_marks(marks)
}

/// Style one wrapped row. The line's kind gives the base style; its marks
/// are what differ. A band — code, or output under an expanded call or a
/// shell line — is padded to `width` so its raised ground covers the whole
/// row. The padding happens only for those kinds at a known width, so
/// nothing else grows trailing cells (a terminal repaints a cell it was
/// not given).
pub fn styled_line(palette: &Palette, row: &RenderedLine, width: Option<usize>) -> UiLine<'static> {
    let base = palette.line(row.kind);
    let text = match (row.kind, width) {
        (LineKind::Code | LineKind::Output, Some(width)) => pad_to_width(&row.text, width),
        _ => row.text.clone(),
    };
    if row.marks.is_empty() {
        return UiLine::from(vec![Span::styled(text, base)]);
    }
    // Marks are sorted and disjoint (the builders emit them left to
    // right); walk the char stream once, splitting on them.
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for &(raw_start, raw_end, mark) in &row.marks {
        let start = raw_start.min(chars.len());
        let end = raw_end.min(chars.len());
        if start > pos {
            spans.push(Span::styled(
                chars[pos..start].iter().collect::<String>(),
                base,
            ));
        }
        if start < end {
            spans.push(Span::styled(
                chars[start..end].iter().collect::<String>(),
                mark_style(palette, mark, base),
            ));
        }
        pos = end.max(pos);
    }
    if pos < chars.len() {
        spans.push(Span::styled(chars[pos..].iter().collect::<String>(), base));
    }
    UiLine::from(spans)
}

/// The style of one marked span on a line of `kind`: the line's own style
/// plus what the mark means.
fn mark_style(palette: &Palette, mark: Mark, base: Style) -> Style {
    use Mark as M;
    match mark {
        M::Bold => base.add_modifier(ratatui::style::Modifier::BOLD),
        M::Crest => palette.user_mark(),
        M::Name => base.add_modifier(ratatui::style::Modifier::BOLD),
        M::Args => Style {
            fg: Some(palette.muted),
            ..base
        },
        M::Good => Style {
            fg: Some(palette.ok),
            ..base
        },
        M::Bad => Style {
            fg: Some(palette.error),
            ..base
        },
    }
}

/// Trailing spaces so a row reaches `width` display cells. Never shortens.
fn pad_to_width(text: &str, width: usize) -> String {
    let missing = width.saturating_sub(text.chars().count());
    let mut padded = String::with_capacity(text.len() + missing);
    padded.push_str(text);
    for _ in 0..missing {
        padded.push(' ');
    }
    padded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::TRUECOLOR;

    fn row(text: &str, kind: LineKind, marks: Vec<(usize, usize, Mark)>) -> RenderedLine {
        RenderedLine::new(text, kind).with_marks(marks)
    }

    fn joined(line: &UiLine<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    }

    #[test]
    fn long_lines_wrap_to_exact_rows_and_marks_ride_the_split() {
        let line = row(
            "0123456789abcdefghij",
            LineKind::Assistant,
            vec![(8, 14, Mark::Bold)],
        );
        let rows = wrap_line(&line, 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].marks, vec![(8, 10, Mark::Bold)]);
        assert_eq!(rows[1].marks, vec![(0, 4, Mark::Bold)]);
        let joined: String = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, line.text);

        // A multibyte char is never cut; a prefix mark stays on row 0.
        let line = row("héllo wörld", LineKind::User, vec![(0, 6, Mark::Crest)]);
        let rows = wrap_line(&line, 5);
        assert_eq!(rows.iter().map(|r| r.text.chars().count()).max(), Some(5));
        assert_eq!(rows[0].marks, vec![(0, 5, Mark::Crest)]);
        assert_eq!(
            rows[1].marks,
            vec![(0, 1, Mark::Crest)],
            "the mark's last char rides the next row, row-local"
        );
    }

    #[test]
    fn a_user_line_splits_its_crest_from_its_prompt() {
        let line = row("you │ hi", LineKind::User, vec![(0, 6, Mark::Crest)]);
        let line = styled_line(&TRUECOLOR, &line, None);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "you │ ");
        assert_eq!(line.spans[0].style.fg, Some(TRUECOLOR.accent));
        assert_eq!(line.spans[1].content, "hi");
        assert_eq!(line.spans[1].style.fg, Some(TRUECOLOR.text));
    }

    #[test]
    fn a_tool_line_styles_its_name_args_and_glyph() {
        let marks = vec![(0, 6, Mark::Name), (6, 9, Mark::Args), (9, 11, Mark::Good)];
        let line = row("▸ read {} ✓", LineKind::Tool, marks);
        let line = styled_line(&TRUECOLOR, &line, None);
        let colors: Vec<_> = line.spans.iter().map(|s| s.style.fg).collect();
        assert_eq!(
            colors,
            vec![
                Some(TRUECOLOR.accent_soft),
                Some(TRUECOLOR.muted),
                Some(TRUECOLOR.ok)
            ]
        );
        // A failed call's line reads the same; its glyph is the error.
        let line = row(
            "▸ read {} ✗",
            LineKind::ToolError,
            vec![(0, 6, Mark::Name), (6, 9, Mark::Args), (9, 11, Mark::Bad)],
        );
        let line = styled_line(&TRUECOLOR, &line, None);
        assert_eq!(line.spans[2].style.fg, Some(TRUECOLOR.error));
        assert_eq!(line.spans[0].style.fg, Some(TRUECOLOR.accent_soft));
    }

    /// A band row — code, or an expanded call's output — reaches the
    /// row's full width, so the raised background covers the whole line.
    /// Nothing else pads.
    #[test]
    fn a_code_row_is_padded_to_the_row_width_and_nothing_else_is() {
        let code = row("let x = 1;", LineKind::Code, vec![]);
        let line = styled_line(&TRUECOLOR, &code, Some(20));
        assert_eq!(joined(&line).chars().count(), 20, "a band, edge to edge");
        assert_eq!(line.spans[0].style.bg, Some(TRUECOLOR.surface_alt));
        assert_eq!(line.spans[0].style.fg, Some(TRUECOLOR.code));

        // The output under an expanded call is a band too, in its colour.
        let output = row("  total 0", LineKind::Output, vec![]);
        let line = styled_line(&TRUECOLOR, &output, Some(20));
        assert_eq!(joined(&line).chars().count(), 20, "output is a band");
        assert_eq!(line.spans[0].style.bg, Some(TRUECOLOR.surface_alt));
        assert_eq!(line.spans[0].style.fg, Some(TRUECOLOR.dim));
        assert_eq!(joined(&styled_line(&TRUECOLOR, &output, None)), "  total 0");

        let prose = row("prose", LineKind::Assistant, vec![]);
        assert_eq!(joined(&styled_line(&TRUECOLOR, &prose, Some(20))), "prose");

        // Width unknown (a test, a zero-width area): no padding guess.
        assert_eq!(joined(&styled_line(&TRUECOLOR, &code, None)), "let x = 1;");
        // A code row longer than the width is not shortened.
        let long = row("0123456789", LineKind::Code, vec![]);
        assert_eq!(
            joined(&styled_line(&TRUECOLOR, &long, Some(4)))
                .chars()
                .count(),
            10
        );
    }

    #[test]
    fn a_zero_width_line_stays_one_row() {
        // The caller's "no width known" case: one row, nothing dropped.
        let rows = wrap_line(&row("abc", LineKind::Assistant, vec![]), 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "abc");
    }

    #[test]
    fn thinking_and_abort_have_their_own_styles() {
        let line = row("~ pondered", LineKind::Thinking, vec![]);
        let style = styled_line(&TRUECOLOR, &line, None).spans[0].style;
        assert_eq!(style.fg, Some(TRUECOLOR.dim));
        assert!(
            style
                .add_modifier
                .contains(ratatui::style::Modifier::ITALIC)
        );

        let line = row(
            "■ run aborted",
            LineKind::Aborted,
            vec![(0, 13, Mark::Crest)],
        );
        let line = styled_line(&TRUECOLOR, &line, None);
        assert_eq!(line.spans[0].style.fg, Some(TRUECOLOR.accent));
        assert!(
            line.spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
}
