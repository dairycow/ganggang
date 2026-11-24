//! Laying a markdown pipe table out at the terminal's width: a grid of
//! padded cells when its natural width fits, a vertical `header: value`
//! list when it does not. The transcript model stays width-free; this is
//! the only place a table becomes text.
//!
//! The grid keeps the markdown delimiter's alignment (`:--` left, `--:`
//! right, `:--:` centre), pads every cell to its column's widest value,
//! and bolds the header under a dim separator. The fallback never needs
//! alignment at all: one bulleted line per row, the remaining cells below
//! it named by their headers, so nothing can shear at any width.

use super::transcript::{Align, LineKind, Mark, RenderedLine, TableRow};

/// Lay a run of table rows out for a terminal `width` columns wide.
pub fn layout(rows: &[TableRow], width: usize) -> Vec<RenderedLine> {
    grid(rows, width).unwrap_or_else(|| list(rows))
}

fn display_width(s: &str) -> usize {
    ratatui::text::Span::raw(s).width()
}

/// The grid, if its natural width fits `width`.
fn grid(rows: &[TableRow], width: usize) -> Option<Vec<RenderedLine>> {
    let columns = rows.first()?.aligns.len();
    if columns == 0 {
        return None;
    }
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| display_width(cell(row, column)))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let needed = widths.iter().sum::<usize>() + 3 * (columns - 1);
    if needed > width {
        return None;
    }

    let mut out = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let text = (0..columns)
            .map(|column| pad(cell(row, column), widths[column], align(row, column)))
            .collect::<Vec<_>>()
            .join(" │ ");
        let end = text.chars().count();
        let mut line = RenderedLine::new(text, LineKind::Table);
        if row.header && end > 0 {
            line = line.with_marks(vec![(0, end, Mark::Bold)]);
        }
        out.push(line);
        if row.header {
            let text = separator(&widths, row);
            let end = text.chars().count();
            let mut sep = RenderedLine::new(text, LineKind::Table);
            if end > 0 {
                sep = sep.with_marks(vec![(0, end, Mark::Args)]);
            }
            out.push(sep);
        }
    }
    Some(out)
}

/// The fallback: one line per non-header row starting with `• `, then one
/// `header: value` line for each further cell. It reads in any width and
/// uses the header row the grid would have shown.
fn list(rows: &[TableRow]) -> Vec<RenderedLine> {
    let headers: Vec<&str> = rows
        .iter()
        .find(|row| row.header)
        .map(|row| row.cells.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    if !rows.iter().any(|row| !row.header) {
        let joined: Vec<&str> = headers.iter().copied().filter(|h| !h.is_empty()).collect();
        if !joined.is_empty() {
            let text = joined.join(" · ");
            let end = text.chars().count();
            out.push(RenderedLine::new(text, LineKind::Table).with_marks(vec![(
                0,
                end,
                Mark::Bold,
            )]));
        }
        return out;
    }
    for row in rows.iter().filter(|row| !row.header) {
        for (column, value) in row.cells.iter().enumerate() {
            let text = if column == 0 {
                format!("• {value}")
            } else if value.is_empty() {
                continue;
            } else {
                match headers.get(column).filter(|header| !header.is_empty()) {
                    Some(header) => format!("  {header}: {value}"),
                    None => format!("  {value}"),
                }
            };
            out.push(RenderedLine::new(text, LineKind::Table));
        }
    }
    out
}

fn cell(row: &TableRow, column: usize) -> &str {
    row.cells.get(column).map(String::as_str).unwrap_or("")
}

fn align(row: &TableRow, column: usize) -> Align {
    row.aligns.get(column).copied().unwrap_or(Align::Left)
}

/// A cell padded to its column width, aligned as the delimiter asked.
fn pad(cell: &str, width: usize, align: Align) -> String {
    let missing = width.saturating_sub(display_width(cell));
    match align {
        Align::Left => format!("{cell}{}", " ".repeat(missing)),
        Align::Right => format!("{}{cell}", " ".repeat(missing)),
        Align::Center => {
            let left = missing / 2;
            format!("{}{cell}{}", " ".repeat(left), " ".repeat(missing - left))
        }
    }
}

/// The rule between a header and its data: dashes per column, colons at
/// the edges that carry alignment, joined like the cells above.
fn separator(widths: &[usize], row: &TableRow) -> String {
    let cells: Vec<String> = widths
        .iter()
        .enumerate()
        .map(|(column, &width)| {
            let align = align(row, column);
            let mut cell: Vec<char> = std::iter::repeat_n('─', width).collect();
            if let Some(first) = cell.first_mut()
                && matches!(align, Align::Left | Align::Center)
            {
                *first = ':';
            }
            if let Some(last) = cell.last_mut()
                && matches!(align, Align::Right | Align::Center)
            {
                *last = ':';
            }
            cell.into_iter().collect()
        })
        .collect();
    cells.join("─┼─")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str], aligns: &[Align], header: bool) -> TableRow {
        TableRow {
            cells: cells.iter().map(|cell| cell.to_string()).collect(),
            aligns: aligns.to_vec(),
            header,
        }
    }

    fn table() -> Vec<TableRow> {
        vec![
            row(&["name", "kind"], &[Align::Left, Align::Left], true),
            row(&["gg", "cli"], &[Align::Left, Align::Left], false),
            row(&["table", "tui"], &[Align::Left, Align::Left], false),
        ]
    }

    #[test]
    fn a_fitting_table_becomes_an_aligned_grid() {
        let lines = layout(&table(), 40);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "name  │ kind",
                ":─────┼─:───",
                "gg    │ cli ",
                "table │ tui ",
            ]
        );
        assert_eq!(lines[0].kind, LineKind::Table);
        assert_eq!(lines[0].marks, vec![(0, 12, Mark::Bold)]);
        assert_eq!(lines[1].marks, vec![(0, 12, Mark::Args)]);
        assert!(lines[2].marks.is_empty());
        assert!(lines.iter().all(|line| line.text.chars().count() == 12));
    }

    #[test]
    fn delimiter_alignment_pads_each_column_its_way() {
        let rows = vec![
            row(
                &["left", "right", "center"],
                &[Align::Left, Align::Right, Align::Center],
                true,
            ),
            row(
                &["a", "b", "c"],
                &[Align::Left, Align::Right, Align::Center],
                false,
            ),
        ];
        let lines = layout(&rows, 40);
        assert_eq!(lines[0].text, "left │ right │ center");
        assert_eq!(lines[2].text, "a    │     b │   c   ");
        assert_eq!(
            lines[2].text.chars().count(),
            lines[0].text.chars().count(),
            "every row shares the grid's width"
        );
        // The rule carries the alignment at its edges.
        assert!(lines[1].text.starts_with(':'), "left edge of a left column");
        assert!(
            lines[1].text.contains("────:"),
            "right edge of a right column: {}",
            lines[1].text
        );
        assert!(lines[1].text.ends_with(':'));
    }

    #[test]
    fn a_table_wider_than_the_terminal_falls_back_to_a_list() {
        let lines = layout(&table(), 10);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["• gg", "  kind: cli", "• table", "  kind: tui"]
        );
        assert!(lines.iter().all(|line| line.kind == LineKind::Table));
    }

    #[test]
    fn the_fallback_keeps_a_headerless_cell_plain() {
        let rows = vec![
            row(&["h", ""], &[Align::Left, Align::Left], true),
            row(&["one", "two"], &[Align::Left, Align::Left], false),
        ];
        let lines = layout(&rows, 4);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["• one", "  two"],
            "an empty header names nothing"
        );
    }

    #[test]
    fn an_empty_table_lays_out_nothing() {
        assert!(layout(&[], 40).is_empty());
    }

    #[test]
    fn wide_cjk_cells_fall_back_to_list_and_header_only_stays_visible() {
        let wide = vec![
            row(&["col", "desc"], &[Align::Left, Align::Left], true),
            row(&["日本語", "説明"], &[Align::Left, Align::Left], false),
        ];
        // Display width is 6 + 3 + 4 = 13 > 11, so it must fall back to list.
        let lines = layout(&wide, 11);
        assert_eq!(lines[0].text, "• 日本語");
        assert_eq!(lines[1].text, "  desc: 説明");

        let header_only = vec![row(&["name", "kind"], &[Align::Left, Align::Left], true)];
        let lines = layout(&header_only, 6);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "name · kind");
        assert_eq!(lines[0].marks, vec![(0, 11, Mark::Bold)]);
    }
}
