//! Tests for [`super`]: the box's height, its scrolling window, the cursor's cell, and what a drawn frame contains.

use super::*;

use crate::tui::layout::Areas;
use crate::tui::theme::TRUECOLOR;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

#[test]
fn the_box_holds_one_to_eight_lines_and_never_more_than_it_is_given() {
    assert_eq!(height(0, 40), 3);
    assert_eq!(height(1, 40), 3);
    assert_eq!(height(4, 40), 6);
    assert_eq!(height(8, 40), 10);
    assert_eq!(height(99, 40), 10, "the window caps at eight");
    assert_eq!(height(99, 4), 4, "a short terminal shrinks the box");
    assert_eq!(height(99, 0), 0, "nothing to spend");
}

#[test]
fn a_draft_of_a_few_lines_is_shown_whole_from_the_top() {
    let areas = Areas::new(Rect::new(0, 0, 80, 24), 0);
    assert_eq!(areas.composer, Rect::new(0, 20, 80, 3));
    let window = Window::new(areas.composer, 1, (0, 0));
    assert_eq!(
        window.area,
        Rect::new(2, 21, 76, 1),
        "padding inside the rules"
    );
    assert_eq!(window.row_start, 0);
    assert_eq!(window.col_start, 0);
    assert_eq!(window.cursor, (2, 21));
}

/// A draft longer than the window: the window scrolls, and the cursor
/// stays on a row the reader can see.
#[test]
fn a_draft_longer_than_the_window_keeps_its_cursor_visible() {
    let areas = Areas::new(Rect::new(0, 0, 80, 24), 12);
    assert_eq!(areas.composer, Rect::new(0, 13, 80, 10), "eight text rows");

    // Cursor on the last of twelve lines: lines 4..=11 show.
    let window = Window::new(areas.composer, 12, (11, 3));
    assert_eq!(window.row_start, 4);
    assert_eq!(window.cursor, (5, 21), "the window's last row, not a rule");
    assert!(
        window.cursor.1 > areas.composer.y && window.cursor.1 < areas.composer.bottom(),
        "inside the box: {window:?}"
    );

    // Back at the top of the same draft: the window follows.
    let window = Window::new(areas.composer, 12, (0, 0));
    assert_eq!(window.row_start, 0);
    assert_eq!(window.cursor, (2, 14));

    // The middle: the window moves just enough.
    let window = Window::new(areas.composer, 12, (9, 40));
    assert_eq!(window.row_start, 2);
    assert_eq!(window.cursor, (42, 21));
}

/// A line wider than the box scrolls from the left rather than
/// wrapping, so the cursor cell stays the truth.
#[test]
fn a_line_wider_than_the_box_scrolls_and_the_cursor_follows() {
    let areas = Areas::new(Rect::new(0, 0, 80, 24), 0);
    let window = Window::new(areas.composer, 1, (0, 200));
    assert_eq!(window.area.width, 76, "two rules, two padding columns");
    assert_eq!(window.col_start, 162, "the cursor near the window's middle");
    assert_eq!(window.cursor, (40, 21));
    assert!(window.cursor.0 >= areas.composer.x + 2);
    assert!(window.cursor.0 < areas.composer.right() - 1);

    // A line that fits is not moved at all.
    let window = Window::new(areas.composer, 1, (0, 70));
    assert_eq!(window.col_start, 0);
    assert_eq!(window.cursor, (72, 21));
}

#[test]
fn a_box_too_small_for_a_cursor_still_places_it_inside() {
    let areas = Areas::new(Rect::new(0, 0, 6, 24), 0);
    let window = Window::new(areas.composer, 1, (0, 999));
    assert_eq!(window.area.width, 2, "two rules, two padding columns");
    assert!(window.cursor.0 >= areas.composer.x + 2);
    assert!(window.cursor.0 < areas.composer.right());
}

#[test]
fn the_window_of_a_line_is_char_wise() {
    assert_eq!(slice("abcdefghij", 3, 4), "defg");
    assert_eq!(slice("abcd", 0, 10), "abcd");
    assert_eq!(slice("abcd", 9, 10), "", "scrolled past the end");
    assert_eq!(slice("héllo wörld", 0, 3), "hél", "no byte is split");
    assert_eq!(slice("你好世界", 1, 2), "好世");
    // A cut that starts mid-row is found in the right row.
    assert_eq!(slice("abcdefghij", 7, 4), "hij");
}

/// The title carries the name and the hint while the box is wide
/// enough, and gives up the hint first when it is not.
#[test]
fn the_title_gives_up_the_hint_before_the_name() {
    let wide = title(&TRUECOLOR, 60);
    let text: String = wide.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.starts_with(" compose "), "{text:?}");
    assert!(text.ends_with(" Enter send · Esc stop "), "{text:?}");
    assert_eq!(text.chars().count(), 60, "the border is filled");

    let tight = title(&TRUECOLOR, 12);
    let text: String = tight.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, " compose ", "the name survives, the hint goes");
    let narrow = title(&TRUECOLOR, 4);
    assert_eq!(narrow.spans[0].content, " com");
}

/// The box itself: rounded rules on `surface`, the placeholder in place
/// of an empty draft, the title on the top border, and the frame's
/// charcoal nowhere near the composer.
#[test]
fn a_frame_draws_the_box_the_title_and_the_placeholder() {
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|frame| {
            let areas = Areas::new(frame.area(), 0);
            draw(
                frame,
                &TRUECOLOR,
                &Editor::default(),
                areas.composer,
                false,
                false,
            );
        })
        .unwrap();
    let rows = rows_of(&terminal);
    let joined = rows.join("\n");
    assert!(joined.contains("╭"), "rounded rules: {joined}");
    assert!(joined.contains(" compose "), "{joined}");
    assert!(joined.contains("Ask gg…"), "{joined}");
    // Every cell of the box is the composer's own ground, including the
    // text row the placeholder does not reach.
    let buffer = terminal.backend().buffer();
    for y in 9..11 {
        for x in 1..59 {
            assert_eq!(
                buffer[(x, y)].style().bg,
                Some(TRUECOLOR.surface),
                "cell ({x}, {y}) is the composer's"
            );
        }
    }
}

/// A row of the test backend's buffer as plain text.
fn rows_of(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let (w, h) = (buffer.area().width, buffer.area().height);
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}
