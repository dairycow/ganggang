//! Minimal markdown rendering for assistant text (spec §15, M7.6):
//! fenced code blocks, bold spans, list markers, and pipe tables —
//! nothing else, hand-rolled. Line-oriented on purpose: the transcript
//! re-renders each frame from the block's full accumulated text, so
//! streaming needs no incremental bookkeeping and an unterminated fence
//! simply renders as code until the stream closes it.
//!
//! Tables need the terminal's width to align their columns, which this
//! pass does not have, so it emits their cells as rows for the view to
//! lay out ([`table`](super::super::table)). Until a table's delimiter row
//! has arrived it renders as the literal lines it is.

use super::render::{Align, LineKind, Mark, RenderedLine, TableRow};

/// Split assistant text into rendered lines. A line whose trimmed form
/// starts with ``` toggles code mode (the fence line itself carries no
/// content; an info string like ```rust is ignored). `**` pairs bold
/// their span; an unpaired trailing marker renders plain. `- `/`* `
/// bullets become `• ` (indent preserved); ordered markers stay typed.
/// A header row followed by a `|---|` delimiter row becomes table rows.
pub(super) fn lines(text: &str) -> Vec<RenderedLine> {
    let source: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut in_code = false;
    let mut i = 0;
    while i < source.len() {
        let line = source[i];
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            out.push(RenderedLine::new(line, LineKind::Code));
            i += 1;
            continue;
        }
        if let Some(aligns) = table_start(&source, i) {
            i = push_table(&source, i, aligns, &mut out);
            continue;
        }
        let displayed = bulletize(line);
        let (plain, bold) = strip_bold(&displayed);
        let marks = bold
            .into_iter()
            .map(|(start, end)| (start, end, Mark::Bold))
            .collect();
        out.push(RenderedLine::new(plain, LineKind::Assistant).with_marks(marks));
        i += 1;
    }
    out
}

/// The alignments of the table starting at `index`, if this line is a
/// header with a matching delimiter row below it. The header and the
/// delimiter must agree on their column count.
fn table_start(source: &[&str], index: usize) -> Option<Vec<Align>> {
    let header = split_row(source.get(index)?)?;
    let aligns = delimiter_aligns(source.get(index + 1)?)?;
    (header.len() == aligns.len()).then_some(aligns)
}

/// Emit the header and every data row of the table at `start`, returning
/// the index after it. Rows end at a blank line, a fence, or a line with
/// no pipe.
fn push_table(
    source: &[&str],
    start: usize,
    aligns: Vec<Align>,
    out: &mut Vec<RenderedLine>,
) -> usize {
    out.push(table_row(
        split_row(source[start]).unwrap_or_default(),
        &aligns,
        true,
    ));
    let mut i = start + 2;
    while i < source.len() {
        let line = source[i];
        if line.trim().is_empty() || line.trim_start().starts_with("```") {
            break;
        }
        let Some(cells) = split_row(line) else {
            break;
        };
        out.push(table_row(cells, &aligns, false));
        i += 1;
    }
    i
}

/// One table row, padded or truncated to the delimiter's column count so
/// every row of a table lines up.
fn table_row(mut cells: Vec<String>, aligns: &[Align], header: bool) -> RenderedLine {
    cells.resize(aligns.len(), String::new());
    // Emphasis markers come off as they do in prose; a cell is one line,
    // so the layout applies the bold to its header row only.
    let cells = cells.into_iter().map(|cell| strip_bold(&cell).0).collect();
    RenderedLine::new(String::new(), LineKind::Table).with_table(TableRow {
        cells,
        aligns: aligns.to_vec(),
        header,
    })
}

/// The cells of a pipe row, or `None` when the line is not a row at all
/// (no pipe). Outer pipes are optional; whitespace around cells is not
/// content. Escaped pipes are not supported.
fn split_row(line: &str) -> Option<Vec<String>> {
    if !line.contains('|') {
        return None;
    }
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    Some(
        trimmed
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

/// The alignments a delimiter row declares, or `None` when the line is
/// not one. Every cell must be dashes with optional alignment colons.
fn delimiter_aligns(line: &str) -> Option<Vec<Align>> {
    let cells = split_row(line)?;
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let left = cell.starts_with(':');
        let right = cell.ends_with(':');
        let body = cell.trim_start_matches(':').trim_end_matches(':');
        if body.is_empty() || !body.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => Align::Center,
            (_, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(aligns)
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
mod tests;
