//! Tests for the frame as drawn: a real [`TestBackend`] buffer at two
//! sizes, checked for the things a reader looks for first — the `gg` mark,
//! the composer's box, the theme's ground under every cell — and for not
//! panicking when the terminal is smaller than the design assumes.
//!
//! These are smoke tests, not pixel snapshots: they hold the drawing
//! together (no panic, the marks present, the frame owned) while leaving
//! the exact glyphs free to change with the design.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::history::History;
use super::theme::TRUECOLOR;
use super::view::{State, render};
use crate::events::AgentEvent;

/// A state with the designed palette, so a test never depends on the
/// environment's `COLORTERM`.
fn fresh(w: u16, h: u16) -> (Terminal<TestBackend>, State) {
    let terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let state = State::with_palette(
        // A cwd that cannot be mistaken for the welcome block's name.
        std::path::Path::new("/home/user/dev/demo"),
        "qwen3.8",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
        TRUECOLOR,
    );
    (terminal, state)
}

/// Draw one frame into the test backend.
fn draw(terminal: &mut Terminal<TestBackend>, state: &mut State) {
    terminal.draw(|frame| render(frame, state)).unwrap();
}

/// One row of the buffer as text, trailing blanks cut.
fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area().width)
        .map(|x| buffer[(x, y)].symbol().trim_end_matches('\u{0}'))
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The whole buffer as text.
fn screen(terminal: &Terminal<TestBackend>) -> String {
    let height = terminal.backend().buffer().area().height;
    (0..height)
        .map(|y| row(terminal, y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The background of every cell, as one set.
fn backgrounds(terminal: &Terminal<TestBackend>) -> Vec<Color> {
    let buffer = terminal.backend().buffer();
    let mut seen: Vec<Color> = Vec::new();
    for y in 0..buffer.area().height {
        for x in 0..buffer.area().width {
            let bg = buffer[(x, y)].style().bg;
            if let Some(bg) = bg
                && !seen.contains(&bg)
            {
                seen.push(bg);
            }
        }
    }
    seen
}

/// The frame contains a span whose text is `needle` — searched in the
/// buffer's cells, which is what a reader sees.
fn shows(terminal: &Terminal<TestBackend>, needle: &str) -> bool {
    screen(terminal).lines().any(|line| line.contains(needle))
}

#[test]
fn an_80_by_24_frame_is_the_theme_not_the_terminal() {
    let (mut terminal, mut state) = fresh(80, 24);
    state.push_user("hello");
    state.apply(&AgentEvent::AssistantText {
        delta: "hi there".into(),
    });
    draw(&mut terminal, &mut state);

    let text = screen(&terminal);
    assert!(shows(&terminal, "gg"), "the footer's mark: {text}");
    assert!(shows(&terminal, "you │ hello"), "{text}");
    assert!(shows(&terminal, "hi there"), "{text}");
    assert!(shows(&terminal, " compose "), "the composer's box: {text}");
    assert!(shows(&terminal, "○ ready"), "the footer's status: {text}");

    // The theme owns the screen: every cell carries a palette colour, and
    // the ground is the charcoal, never the terminal's own default.
    let allowed = [
        TRUECOLOR.bg,
        TRUECOLOR.surface,
        TRUECOLOR.surface_alt,
        TRUECOLOR.selection,
    ];
    for bg in backgrounds(&terminal) {
        assert!(allowed.contains(&bg), "unexpected background {bg:?}");
    }
    assert!(
        backgrounds(&terminal).contains(&TRUECOLOR.bg),
        "the transcript sits on charcoal"
    );
    assert!(
        backgrounds(&terminal).contains(&TRUECOLOR.surface),
        "the composer and footer sit on surface"
    );
    assert_eq!(
        *terminal.backend().buffer().area(),
        Rect::new(0, 0, 80, 24),
        "the frame fills the terminal"
    );

    // The footer is one row of the composer's own ground, edge to edge —
    // not just behind the words.
    let buffer = terminal.backend().buffer();
    for x in 0..80 {
        assert_eq!(
            buffer[(x, 23)].style().bg,
            Some(TRUECOLOR.surface),
            "footer cell ({x}, 23)"
        );
    }
}

/// A 24×8 terminal is a real thing (a narrow split, a phone terminal): the
/// layout must adapt, keep the marks, and not panic. The composer may
/// shrink to its minimum; the transcript keeps a row.
#[test]
fn a_24_by_8_frame_adapts_without_losing_the_marks() {
    let (mut terminal, mut state) = fresh(24, 8);
    state
        .editor
        .set_text("a draft\nthat is\nlong enough to\nneed scrolling");
    draw(&mut terminal, &mut state);

    let text = screen(&terminal);
    assert!(shows(&terminal, "gg"), "the mark fits: {text}");
    assert!(shows(&terminal, " compose"), "the box is named: {text}");
    let rows: Vec<String> = (0..8).map(|y| row(&terminal, y)).collect();
    assert!(
        rows.iter().any(|r| r.contains('╰') || r.contains('╯')),
        "the composer's box is closed inside 8 rows: {text}"
    );
    // Nothing reaches past the terminal, in either direction.
    for line in &rows {
        assert!(line.chars().count() <= 24, "row too wide: {line:?}");
    }
}

/// A terminal with nothing in it: the welcome block, and no chrome.
#[test]
fn an_empty_session_draws_the_welcome_block() {
    let (mut terminal, mut state) = fresh(80, 24);
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "ganggang"), "{text}");
    assert!(shows(&terminal, "a small, sharp agentic harness"), "{text}");
    assert!(shows(&terminal, "Enter send · / commands"), "{text}");

    // One line of anything, and it is gone: the block belongs to an empty
    // session, not to the transcript area.
    state.push_info("one line");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "one line"), "{text}");
    assert!(!shows(&terminal, "ganggang"), "{text}");
    assert!(!shows(&terminal, "a small, sharp"), "{text}");

    // A resumed session has a conversation, whatever its blocks render:
    // no welcome for it either.
    let (mut terminal, mut state) = fresh(80, 24);
    state.seed_from_entries(&[crate::agent::session::Entry::Message {
        id: "e1".into(),
        parent_id: None,
        timestamp: String::new(),
        message: crate::agent::session::Message {
            role: crate::agent::session::Role::User,
            content: Some("earlier question".into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
            is_error: false,
            reasoning: None,
        },
    }]);
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "earlier question"), "{text}");
    assert!(!shows(&terminal, "ganggang"), "{text}");
}

/// A transcript longer than the view earns a scrollbar in the transcript's
/// own column — and a transcript that fits does not.
#[test]
fn a_transcript_that_overflows_shows_a_scrollbar_and_one_that_fits_does_not() {
    let (mut terminal, mut state) = fresh(80, 24);
    state.apply(&AgentEvent::AssistantText {
        delta: "one line".into(),
    });
    draw(&mut terminal, &mut state);
    let buffer = terminal.backend().buffer();
    // The transcript's own rows only: the composer's rule is its own.
    let chrome: Vec<&str> = (0..20).map(|y| buffer[(79u16, y)].symbol()).collect();
    assert!(
        chrome.iter().all(|s| s.trim().is_empty()),
        "content that fits shows no scrollbar: {chrome:?}"
    );

    // Twenty-five lines in a 20-row transcript: five rows are hidden, so
    // the bar shows — before anyone has scrolled.
    let (mut terminal, mut state) = fresh(80, 24);
    for i in 0..25 {
        state.apply(&AgentEvent::TurnStarted);
        state.apply(&AgentEvent::AssistantText {
            delta: format!("line {i}"),
        });
    }
    // The bar lives in the transcript's last column, which the text does
    // not reach: nothing is drawn under it.
    let column = 79u16;
    draw(&mut terminal, &mut state);
    let symbols: Vec<&str> = (0..20)
        .map(|y| terminal.backend().buffer()[(column, y)].symbol())
        .collect();
    assert!(
        symbols.iter().any(|s| s.contains('║') || s.contains('▮')),
        "a thumb or a track in the scrollbar column: {symbols:?}"
    );

    // Where the thumb is, read off the buffer by its colour: the view's
    // share of the track (16 of 20 rows), ending on the track's last row
    // while the newest line shows, and starting on its first row once the
    // first line does. In between, in between.
    let thumb_rows = |terminal: &Terminal<TestBackend>| -> Vec<u16> {
        let buffer = terminal.backend().buffer();
        (0..20u16)
            .filter(|y| buffer[(column, *y)].style().fg == Some(TRUECOLOR.accent))
            .collect()
    };
    assert_eq!(
        thumb_rows(&terminal),
        (4..20).collect::<Vec<_>>(),
        "at rest the thumb ends on the track's last row"
    );
    state.scroll_up(2);
    draw(&mut terminal, &mut state);
    assert_eq!(
        thumb_rows(&terminal),
        (2..18).collect::<Vec<_>>(),
        "two rows up: two rows up"
    );

    // Past the first row there is nothing to scroll to: the offset stops
    // there, the thumb with it, and the text does not detach from the
    // composer.
    state.scroll_up(100);
    draw(&mut terminal, &mut state);
    assert_eq!(
        state.offset_for_test(),
        5,
        "five rows were hidden, so five rows is all there is to scroll"
    );
    assert_eq!(
        thumb_rows(&terminal),
        (0..16).collect::<Vec<_>>(),
        "at the first row the thumb starts on the first"
    );
    let text = screen(&terminal);
    assert!(shows(&terminal, "line 0"), "{text}");
    assert!(!shows(&terminal, "line 24"), "{text}");
    assert!(
        row(&terminal, 19).contains("line 19"),
        "the last transcript row is still text, not a gap: {text}"
    );
}

/// The popup: a floating panel on `surface_alt`, its selected row marked
/// with a scarlet marker rather than a terminal inversion.
#[test]
fn the_suggestion_popup_is_a_panel_of_its_own() {
    let (mut terminal, mut state) = fresh(80, 24);
    state.editor.set_text("/re");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "/resume [n|prefix]"), "{text}");
    assert!(shows(&terminal, "▸ /resume"), "the marker: {text}");
    assert!(shows(&terminal, "↑↓ select"), "{text}");
    // The selection is the theme's `selection`, not an inversion — so a
    // 256-colour terminal gets the same look, and the terminal's own
    // colours stay out of it.
    let buffer = terminal.backend().buffer();
    let mut saw_selection = false;
    let mut selected_rows: Vec<u16> = Vec::new();
    for y in 0..buffer.area().height {
        let mut whole_row = true;
        for x in 0..buffer.area().width {
            let style = buffer[(x, y)].style();
            if style.bg == Some(TRUECOLOR.selection) {
                saw_selection = true;
            } else {
                whole_row = false;
            }
            assert!(
                !style
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED),
                "no inverted cells: the theme draws the selection"
            );
        }
        if whole_row {
            selected_rows.push(y);
        }
    }
    assert!(saw_selection, "the selected row is on `selection`");
    // The row that carries the marker is the whole of the selection:
    // edge to edge, and the only row that is.
    let marker_row = (0..buffer.area().height).find(|y| buffer[(0, *y)].symbol() == "▸");
    assert_eq!(
        selected_rows,
        marker_row.into_iter().collect::<Vec<_>>(),
        "the selected row is selected edge to edge, the only row that is"
    );
}

/// A palette is chosen once, and a 256-colour session draws with indexed
/// colours throughout — the real fallback, not a stand-in for it: no
/// truecolor escape ever reaches the terminal.
#[test]
fn an_indexed_session_draws_only_indexed_colours() {
    // What a terminal without `COLORTERM=truecolor` gets.
    let indexed = super::theme::indexed_palette();
    for (what, color) in [
        ("bg", indexed.bg),
        ("surface", indexed.surface),
        ("accent", indexed.accent),
        ("selection", indexed.selection),
    ] {
        assert!(matches!(color, Color::Indexed(_)), "{what} is {color:?}");
    }
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    let mut state = State::with_palette(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
        indexed,
    );
    state.push_user("hi");
    state.apply(&AgentEvent::AssistantText {
        delta: "```rust\nfn a() {}\n```".into(),
    });
    state.apply(&AgentEvent::ToolCallStarted {
        name: "bash".into(),
        id: "c".into(),
        args: serde_json::json!({"command": "ls"}),
    });
    state.apply(&AgentEvent::Warned {
        message: "careful".into(),
    });
    state.begin_run();
    draw(&mut terminal, &mut state);

    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area().height {
        for x in 0..buffer.area().width {
            let style = buffer[(x, y)].style();
            for color in [style.fg, style.bg].into_iter().flatten() {
                assert!(
                    matches!(color, Color::Indexed(_) | Color::Reset),
                    "cell ({x}, {y}) asks for {color:?} in an indexed session"
                );
            }
        }
    }
    let text = screen(&terminal);
    assert!(
        shows(&terminal, "▸ bash"),
        "the tool line still reads: {text}"
    );
    assert!(
        shows(&terminal, "fn a() {}"),
        "the code band still reads: {text}"
    );
    assert!(shows(&terminal, "⠋ working"), "the spinner shows: {text}");
}

/// Every row of the transcript is the theme's, and a wrapped row keeps the
/// style its line had — the check that styling rides the wrap.
#[test]
fn a_code_block_reads_as_a_band_across_the_transcript() {
    let (mut terminal, mut state) = fresh(40, 12);
    state.apply(&AgentEvent::AssistantText {
        delta: "before\n```\nshort\n```\nafter".into(),
    });
    draw(&mut terminal, &mut state);
    let buffer = terminal.backend().buffer();
    let y = (0..buffer.area().height)
        .find(|y| row(&terminal, *y).contains("short"))
        .unwrap();
    // Every cell of the row, the last column included.
    for x in 0..40 {
        assert_eq!(
            buffer[(x, y)].style().bg,
            Some(TRUECOLOR.surface_alt),
            "cell ({x}, {y}) is part of the code band"
        );
    }
}

/// Output under an expanded call is a band too: its raised ground covers
/// the whole row, whatever the text's length.
#[test]
fn an_expanded_call_output_reads_as_a_band() {
    let (mut terminal, mut state) = fresh(40, 12);
    state.apply(&AgentEvent::ToolCallStarted {
        name: "bash".into(),
        id: "c".into(),
        args: serde_json::json!({"command": "echo hi"}),
    });
    state.apply(&AgentEvent::ToolCallFinished {
        name: "bash".into(),
        id: "c".into(),
        output: "hi".into(),
        is_error: false,
    });
    state.toggle_expand();
    draw(&mut terminal, &mut state);
    // The output's own row — not the call's line above it, whose
    // arguments quote the same word.
    let buffer = terminal.backend().buffer();
    let y = (0..buffer.area().height)
        .find(|y| row(&terminal, *y).trim() == "hi")
        .unwrap();
    for x in 0..40 {
        assert_eq!(
            buffer[(x, y)].style().bg,
            Some(TRUECOLOR.surface_alt),
            "cell ({x}, {y}) is part of the output band"
        );
    }
}

/// Nothing about a frame's styling depends on the terminal being a
/// comfortable size: zero and one-cell terminals must not panic.
#[test]
fn degenerate_terminals_do_not_panic() {
    for (w, h) in [(1, 1), (4, 2), (5, 3), (80, 1), (1, 40), (3, 6)] {
        let (mut terminal, mut state) = fresh(w, h);
        state.push_user("a user line long enough to wrap several times over");
        state.apply(&AgentEvent::AssistantText {
            delta: "```\ncode\n```".into(),
        });
        state.editor.set_text("draft\nsecond line");
        state.begin_run();
        state.scroll_up(5);
        draw(&mut terminal, &mut state);
    }
}
