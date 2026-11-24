//! The composer: its box, its height, its scrolling window, and its
//! cursor.
//!
//! Everything about the compose box that can be wrong without an error —
//! a cursor that lands on a border, a box that grows until the transcript
//! vanishes, a draft whose cursor row is hidden below the fold — lives
//! here, as numbers a test can check. The box itself is a rounded border
//! on `surface`; the border goes warm while a run is live and scarlet
//! while it is being stopped, which is how a reader knows the composer is
//! gg's rather than the terminal's.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::input::Editor;
use super::theme::Palette;

/// The composer's text window: from one typed line to eight. A draft
/// longer than that scrolls inside the window (see [`Window::new`]).
pub const MAX_LINES: usize = 8;

/// The composer's border rows: the title row above, the rule below.
pub const BORDERS: u16 = 2;

/// One column of inner padding each side, so text never touches the rule.
const PAD: u16 = 1;

/// The composer's name, on its top border.
const TITLE: &str = " compose ";

/// The composer's right-hand hint, on the same border.
const HINT: &str = " Enter send · Esc stop ";

/// What an empty composer says. One line, and the two things a reader is
/// most likely to want: that `/` opens the commands, and that a long draft
/// can go to a real editor.
pub const PLACEHOLDER: &str = "Ask gg…  / commands · Ctrl+G editor";

/// The composer's box height for a draft of `lines` logical lines: one to
/// eight text rows plus the two border rows, never more than `available`.
/// An empty draft still gets a full row — its placeholder has to sit
/// somewhere.
pub fn height(lines: usize, available: u16) -> u16 {
    let text = if lines == 0 { 1 } else { lines.min(MAX_LINES) };
    (text as u16).saturating_add(BORDERS).min(available)
}

/// Where a draft's rows and its cursor are inside the box: which logical
/// lines show, which columns of them, and the cursor's absolute cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Where the visible rows render (inside the borders, one column of
    /// padding off each rule).
    pub area: Rect,
    /// First logical line the window shows.
    pub row_start: usize,
    /// First column the window shows — shared by every visible line, so a
    /// long line reads the same way whichever line the cursor is on.
    pub col_start: usize,
    /// The terminal cursor, in absolute cells.
    pub cursor: (u16, u16),
}

impl Window {
    /// Place a draft of `lines` logical lines with its cursor at `cursor`
    /// (logical row, column) inside `box_`.
    ///
    /// The window follows the cursor when the draft overflows the box, in
    /// both directions: rows scroll to keep the cursor's line on show, and
    /// columns to keep its column on show. A line longer than the box is
    /// cut from the left rather than soft-wrapped — which is what keeps
    /// the cursor's cell exact, the thing a composer is judged on.
    pub fn new(box_: Rect, lines: usize, cursor: (usize, usize)) -> Self {
        // A box too narrow for even one text column has none: the area is
        // empty, and [`cursor_position`] says there is nowhere to put the
        // cursor. (Padding it to a column would put text on, or past, the
        // right rule.)
        let width = box_.width.saturating_sub(2 + 2 * PAD);
        let height = box_.height.saturating_sub(BORDERS);
        let area = Rect {
            x: box_.x.saturating_add(1).saturating_add(PAD),
            y: box_.y.saturating_add(1),
            width,
            height,
        };
        let row_start = scrolled_row(cursor.0, height as usize, lines);
        // The row inside the window, clamped so the cursor cannot walk
        // onto a border when the box is shorter than the draft.
        let shown = cursor
            .0
            .saturating_sub(row_start)
            .min(height.saturating_sub(1) as usize);
        let col_start = scrolled_col(cursor.1, width as usize);
        Self {
            area,
            row_start,
            col_start,
            cursor: (
                area.x.saturating_add((cursor.1 - col_start) as u16),
                area.y.saturating_add(shown as u16),
            ),
        }
    }
}

/// The first logical line a window of `rows` shows for a cursor on line
/// `cursor_row` of a `lines`-line draft: anchored at the top while the
/// draft fits, then the smallest scroll that keeps the cursor visible —
/// and never past the last full window.
fn scrolled_row(cursor_row: usize, rows: usize, lines: usize) -> usize {
    if rows == 0 || lines <= rows {
        return 0;
    }
    cursor_row.saturating_sub(rows - 1).min(lines - rows)
}

/// The first column of a line `width` cells wide that keeps a cursor at
/// `column` visible: no scroll while the line fits, then the cursor sits
/// near the middle so both ends of what is around it stay readable.
fn scrolled_col(column: usize, width: usize) -> usize {
    if width == 0 || column < width {
        return 0;
    }
    column - width / 2
}

/// Draw the composer: box, title, draft window, cursor.
pub fn draw(
    frame: &mut Frame,
    palette: &Palette,
    editor: &Editor,
    box_: Rect,
    busy: bool,
    stopping: bool,
) {
    let (fill, border) = palette.composer(busy, stopping);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(fill)
        .border_style(border)
        // The title's area is the border less its two corners.
        .title_top(title(palette, box_.width.saturating_sub(2)));
    let window = Window::new(box_, editor.line_count(), editor.cursor_row_col());
    let lines = visible_lines(palette, editor, &window);
    frame.render_widget(block, box_);
    frame.render_widget(Paragraph::new(lines).style(fill), window.area);
    // A box with no text row or column has nowhere to put a cursor; setting
    // one anyway would park it on a rule or on another widget's cells.
    if let Some(cell) = cursor_position(box_, &window) {
        frame.set_cursor_position(cell);
    }
}

/// Where the terminal cursor goes for a draft in `box_`: the window's cell,
/// but only strictly inside the rules. `None` for a box with no text row
/// (two rows are both border) or no text column — a short or narrow
/// terminal shrinks the composer to exactly that, and a cursor on the
/// bottom rule reads as a cursor that has escaped the box.
pub fn cursor_position(box_: Rect, window: &Window) -> Option<(u16, u16)> {
    if window.area.width == 0 || window.area.height == 0 {
        return None;
    }
    let (x, y) = window.cursor;
    let inside = x > box_.left()
        && x < box_.right().saturating_sub(1)
        && y > box_.top()
        && y < box_.bottom().saturating_sub(1);
    inside.then_some((x, y))
}

/// The composer's title row: what the box is, and how to drive it at the
/// far right. It gives up the hint before it gives up the name, and gives
/// up both before it overflows the border.
fn title(palette: &Palette, width: u16) -> UiLine<'static> {
    // Char counts, not bytes: the middot in the hint is two bytes, and
    // counting them would leave a border one cell short.
    let width = width as usize;
    let (name, hint) = (TITLE.chars().count(), HINT.chars().count());
    if width < name + hint {
        return UiLine::from(Span::styled(
            TITLE.chars().take(width).collect::<String>(),
            Style::new().fg(palette.muted),
        ));
    }
    let gap = " ".repeat(width - name - hint);
    UiLine::from(vec![
        Span::styled(TITLE, Style::new().fg(palette.muted)),
        Span::raw(gap),
        Span::styled(HINT, Style::new().fg(palette.dim)),
    ])
}

/// The draft as the window shows it: `window`'s rows and columns, with the
/// placeholder standing in for an empty composer. The rows fill the
/// window's height so the box's background is its own, edge to edge.
fn visible_lines(palette: &Palette, editor: &Editor, window: &Window) -> Vec<UiLine<'static>> {
    let all = editor.lines();
    let empty = editor.text().is_empty();
    let width = window.area.width as usize;
    let body = Style::new().fg(palette.text).bg(palette.surface);
    let placeholder = body.fg(palette.dim).add_modifier(Modifier::ITALIC);
    (0..window.area.height as usize)
        .map(|row| {
            // An empty composer has no text to window: the placeholder
            // starts at the left, where a reader looks first.
            let text = if empty && row == 0 {
                slice(PLACEHOLDER, 0, width)
            } else {
                match all.get(window.row_start + row) {
                    Some(line) => slice(line, window.col_start, width),
                    None => String::new(),
                }
            };
            let style = if empty && row == 0 { placeholder } else { body };
            UiLine::from(Span::styled(text, style))
        })
        .collect()
}

/// `width` chars of `line` from `from`: the horizontal window. Char-wise
/// like [`wrap_line`](crate::tui::line::wrap_line), so a multibyte char is
/// never split.
fn slice(line: &str, from: usize, width: usize) -> String {
    line.chars().skip(from).take(width).collect()
}
#[cfg(test)]
mod tests;
