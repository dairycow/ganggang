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
    assert!(shows(&terminal, "> hello"), "{text}");
    // Two speakers, two colours: the user's mark is the scarlet crest and
    // the model's text is cream, so the two never read as one.
    let buffer = terminal.backend().buffer();
    let colour_of = |symbol: &str| {
        (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].clone())
            .find(|cell| cell.symbol() == symbol)
            .map(|cell| cell.fg)
    };
    assert_eq!(colour_of(">"), Some(TRUECOLOR.accent));
    assert_eq!(colour_of("h"), Some(TRUECOLOR.text));
    assert!(shows(&terminal, "hi there"), "{text}");
    assert!(shows(&terminal, " compose "), "the composer's box: {text}");
    // The footer at rest: the name leads the row, and the row has no
    // status word.
    let footer = row(&terminal, 23);
    assert!(footer.starts_with("gg │ "), "the footer's mark: {footer:?}");
    assert!(
        !footer.contains("ready"),
        "no status word at rest: {footer:?}"
    );

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

/// `gg` sits at the left edge of the footer, at rest and on every frame
/// of a run: the glyphs never change, so the row's text is the rest row
/// in every frame, and the colour the mark wears, not the glyphs, says
/// a run is live. It cycles the crest's three reds, one per frame. No
/// dot and no ring is drawn anywhere in the row.
#[test]
fn the_mark_is_gg_at_rest_and_while_a_run_works() {
    use crate::tui::footer::MARK_FRAMES as FRAMES;

    // Idle: the mark is the name, at the left edge of the footer's row,
    // in the crest's scarlet.
    let (mut terminal, mut state) = fresh(80, 24);
    draw(&mut terminal, &mut state);
    let rest = row(&terminal, 23);
    assert!(
        rest.starts_with("gg │ "),
        "the mark sits at the left of the footer: {rest:?}"
    );

    // A run is live: the row reads as the rest row, frame after frame,
    // and the mark's colour is all that moves.
    state.begin_run();
    let mut seen = Vec::new();
    for frame in 0..FRAMES.len() as u16 {
        state.tick_spinner();
        draw(&mut terminal, &mut state);
        let footer = row(&terminal, 23);
        assert_eq!(
            footer, rest,
            "frame {frame} reads as the rest row: {footer:?}"
        );
        assert!(
            !footer.contains('●') && !footer.contains('○'),
            "frame {frame} draws no dot and no ring: {footer:?}"
        );
        assert!(
            !footer.contains("working") && !footer.contains("ready"),
            "frame {frame} carries no status word: {footer:?}"
        );
        seen.push(terminal.backend().buffer()[(0, 23)].style().fg);
    }
    // The run began on the crest, so the drawn frames come round soft,
    // deep, scarlet: the beat is the crest's three reds, one per frame.
    let p = state.palette();
    assert_eq!(
        seen,
        vec![Some(p.accent_soft), Some(p.accent_deep), Some(p.accent)],
        "the crest's three reds, one per drawn frame: {seen:?}"
    );

    // The run ends and the mark sits still in its crest red.
    state.run_finished();
    draw(&mut terminal, &mut state);
    let footer = row(&terminal, 23);
    assert_eq!(footer, rest, "the row is the rest row again: {footer:?}");
    let fg = terminal.backend().buffer()[(0, 23)].style().fg;
    assert_eq!(fg, Some(p.accent), "the mark rests in the crest's scarlet");
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
        symbols.iter().all(|s| s.contains('▐')),
        "the lane and its thumb are the same half-block, on the right: {symbols:?}"
    );
    // The outline: every cell of the bar is black on its left half, the
    // text's ground is not, and the thumb is scarlet where the track is
    // the border colour.
    let cell = |y: u16| terminal.backend().buffer()[(column, y)].clone();
    assert_eq!(cell(4).bg, TRUECOLOR.outline);
    assert_eq!(cell(4).fg, TRUECOLOR.accent);
    assert_eq!(cell(1).fg, TRUECOLOR.border);
    assert_eq!(buffer[(78, 4)].bg, TRUECOLOR.bg);

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

/// Home puts the session's first row at the top of the transcript; End
/// puts the newest row back, where new output can follow it.
#[test]
fn home_scrolls_to_the_first_row_and_end_back_to_the_newest() {
    let (mut terminal, mut state) = fresh(80, 24);
    for i in 0..25 {
        state.apply(&AgentEvent::TurnStarted);
        state.apply(&AgentEvent::AssistantText {
            delta: format!("line {i}"),
        });
    }
    draw(&mut terminal, &mut state);
    assert!(
        row(&terminal, 19).contains("line 24"),
        "at rest the newest row is the transcript's last: {}",
        screen(&terminal)
    );

    state.scroll_to_top();
    draw(&mut terminal, &mut state);
    assert!(
        row(&terminal, 0).contains("line 0"),
        "Home: the session's first row is the transcript's first: {}",
        screen(&terminal)
    );
    assert!(
        !shows(&terminal, "line 24"),
        "Home leaves the newest row below the fold: {}",
        screen(&terminal)
    );

    state.scroll_to_end();
    draw(&mut terminal, &mut state);
    assert!(
        row(&terminal, 19).contains("line 24"),
        "End: the newest row is the transcript's last again: {}",
        screen(&terminal)
    );
    assert!(
        !shows(&terminal, "line 0"),
        "End scrolls the first row away: {}",
        screen(&terminal)
    );
}

/// The popup: a floating panel on `surface_alt`, its selected row marked
/// with a scarlet marker rather than a terminal inversion.
#[test]
fn the_suggestion_popup_is_a_panel_of_its_own() {
    let (mut terminal, mut state) = fresh(80, 24);
    state.editor.set_text("/se");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "/session [filter|copy|info]"), "{text}");
    assert!(shows(&terminal, "▸ /session"), "the marker: {text}");
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

/// Open the list on `sessions` (a loaded listing, with `/s` as this
/// directory's own and `/home/u` as home) on the composer's `text`, the
/// highlight on `selected`, and draw: the listing is in place before the
/// text opens the list, as in the loop.
fn open_list(
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    state: &mut State,
    sessions: Vec<crate::agent::session::io::SessionFile>,
    text: &str,
    selected: usize,
) {
    state.editor.set_text("");
    state.sync_picker();
    state.sessions = super::picker::tests::listing_of(sessions);
    state.editor.set_text(text);
    state.sync_picker();
    state.picker.as_mut().unwrap().selected = selected;
    draw(terminal, state);
}

/// The list's panel is a window over the listing that follows the
/// highlight, on the same floating panel as the suggestion popup. The
/// hint row says where the highlight stands; a scope with no row at all
/// says why on its own row; a failed open says its error on the panel's
/// last row, in the hint row's place.
#[test]
fn the_list_is_a_window_that_follows_the_highlight() {
    let (mut terminal, mut state) = fresh(80, 24);
    open_list(
        &mut terminal,
        &mut state,
        super::picker::tests::sessions_for_test(30),
        "/session",
        25,
    );

    // An empty draft is a three-row composer over a one-row footer, so
    // the panel has the 20 rows above it: 19 rows of sessions, then the
    // hint. The highlighted row is the window's last, and the highlight
    // is the whole row, edge to edge.
    let marker_row = (0..24).find(|y| row(&terminal, *y).starts_with('▸'));
    assert_eq!(marker_row, Some(18));
    let text = screen(&terminal);
    assert!(shows(&terminal, "s25"), "{text}");
    assert!(
        shows(&terminal, "s07"),
        "the window starts 18 rows above: {text}"
    );
    assert!(
        !shows(&terminal, "s06"),
        "nothing from above the window: {text}"
    );
    assert!(!shows(&terminal, "s26"), "or below it: {text}");
    let hint = row(&terminal, 19);
    assert!(
        hint.starts_with(" ↑↓ move 26/30 · "),
        "the highlight's place in the list: {hint}"
    );
    assert!(hint.ends_with("Esc clears"), "{hint}");
    assert!(
        row(&terminal, 20).contains(" compose "),
        "the composer is untouched below the panel: {text}"
    );
    {
        let buffer = terminal.backend().buffer();
        for x in 0..80 {
            assert_eq!(
                buffer[(x, 18)].style().bg,
                Some(TRUECOLOR.selection),
                "cell ({x}, 18) is on the highlight"
            );
        }
    }

    // Back at the top, the window is anchored there.
    state.picker.as_mut().unwrap().selected = 0;
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(row(&terminal, 0).contains("s00"), "{text}");
    assert!(row(&terminal, 18).contains("s18"), "{text}");
    assert!(row(&terminal, 19).starts_with(" ↑↓ move 1/30 · "), "{text}");

    // A list that fits is a short panel directly above the composer, and
    // its hint has no counter: this directory's three.
    open_list(
        &mut terminal,
        &mut state,
        super::picker::tests::sessions_for_test(3),
        "/session",
        0,
    );
    state.session_scope_this_directory();
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(row(&terminal, 16).contains("s00"), "{text}");
    assert!(row(&terminal, 18).contains("s02"), "{text}");
    assert_eq!(
        row(&terminal, 19),
        " ↑↓ move · type filters · Tab: all directories · Enter picks · Esc clears"
    );
    assert!(
        !row(&terminal, 15).contains("↑↓"),
        "nothing above the panel: {text}"
    );

    // Every directory's sessions: each row shows its session's directory
    // in the model's place, and the hint offers the way back to this
    // directory's. Five sessions make a panel six high, its first row at
    // 14 (no filter: the action rows stay away).
    let everywhere = [
        super::picker::tests::sessions_for_test(3),
        super::picker::tests::elsewhere_for_test(2),
    ]
    .concat();
    state.close_picker();
    open_list(&mut terminal, &mut state, everywhere, "/session", 0);
    let text = screen(&terminal);
    assert!(row(&terminal, 14).contains("s00"), "{text}");
    assert!(
        row(&terminal, 16).contains("/tmp/x"),
        "this directory's sessions name their directory too: {text}"
    );
    assert!(
        row(&terminal, 17).contains("~/repo/.worktrees/wt00"),
        "{text}"
    );
    assert!(
        row(&terminal, 18).contains("~/repo/.worktrees/wt01"),
        "{text}"
    );
    assert!(row(&terminal, 19).contains("Tab: this directory"), "{text}");
    // And back again.
    state.session_scope_this_directory();
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(row(&terminal, 16).contains("s00"), "{text}");
    assert!(shows(&terminal, "s01"), "{text}");
    assert!(
        row(&terminal, 19).contains("Tab: all directories"),
        "{text}"
    );

    // A filter that keeps no row at all still draws the panel, in two
    // rows: the empty row that says why, then the hint. The panel floats
    // over the transcript's newest rows, so it cannot vanish and say
    // nothing. The narrow scope (this directory's sessions) is in view:
    // the empty row is at the panel's top, the hint just above the
    // composer.
    state.editor.set_text("/session zzz");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert_eq!(
        row(&terminal, 18),
        " no session matches \"zzz\"",
        "the panel says why the list is empty: {text}"
    );
    assert_eq!(
        row(&terminal, 19),
        " ↑↓ move · type filters · Tab: all directories · Enter picks · Esc clears",
        "the hint row stays the last: {text}"
    );
    assert!(!shows(&terminal, "s00"), "{text}");
    assert!(shows(&terminal, "/session zzz"), "{text}");

    // Tab into a directory with no sessions: its own empty row, over a
    // hint that offers the wider listing.
    state.close_picker();
    open_list(
        &mut terminal,
        &mut state,
        super::picker::tests::elsewhere_for_test(2),
        "/session",
        0,
    );
    state.session_scope_this_directory();
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert_eq!(
        row(&terminal, 18),
        " no sessions in this directory",
        "{text}"
    );
    assert!(
        row(&terminal, 19).contains("Tab: all directories"),
        "the hint row offers the wider listing: {text}"
    );

    // A failed open says its error on the panel's last row, in the hint
    // row's place; a changed filter is a new list, and brings the hint
    // back.
    state.close_picker();
    open_list(
        &mut terminal,
        &mut state,
        super::picker::tests::sessions_for_test(3),
        "/session",
        0,
    );
    state.session_scope_this_directory();
    state.pick_failed("boom");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert_eq!(
        row(&terminal, 19),
        "⚠ boom",
        "the error takes the hint row's place: {text}"
    );
    assert!(!shows(&terminal, "↑↓ move"), "{text}");
    assert!(shows(&terminal, "s00"), "the rows stay: {text}");
    state.editor.set_text("/session s01");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert_eq!(
        row(&terminal, 19),
        " ↑↓ move · type filters · Tab: all directories · Enter picks · Esc clears",
        "a new list brings the hint row back: {text}"
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
    // The footer is the last row: the mark leads it, and the row is the
    // rest row while a run works, the beat being colour only.
    let footer = row(&terminal, 15);
    assert_eq!(
        footer, "gg │ …/tmp/x │ m │ ↑0 ↓0",
        "the mark leads the footer in the name's two cells: {footer:?}"
    );
    assert!(!footer.contains('●'), "no dot in the footer: {footer:?}");
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
    // The picker too, over a list longer than any of them can show.
    for (w, h) in [(1, 1), (4, 2), (5, 3), (80, 1), (1, 40), (3, 6)] {
        let (mut terminal, mut state) = fresh(w, h);
        state.picker = Some(super::picker::tests::pick_for_test(30));
        state.picker_next();
        state.editor.set_text("s");
        draw(&mut terminal, &mut state);
    }
    // And the list's empty scope, with no row at all to show.
    for (w, h) in [(1, 1), (4, 2), (5, 3), (80, 1), (1, 40), (3, 6)] {
        let (mut terminal, mut state) = fresh(w, h);
        state.editor.set_text("/session");
        state.sync_picker();
        state.picker_next();
        draw(&mut terminal, &mut state);
    }
}

/// The composer's hint follows the queue: what Enter queues while a run
/// works, what ↑ takes back, and what a held queue says it waits for.
#[test]
fn the_composer_hint_follows_the_queue() {
    // A run in flight, nothing queued.
    let (mut terminal, mut state) = fresh(80, 24);
    state.begin_run();
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "Enter queue · Esc stop"), "{text}");

    // A run in flight, items queued: ↑ can take one back.
    state.queue_prompt("later");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(
        shows(&terminal, "Enter queue · ↑ take back · Esc stop"),
        "{text}"
    );

    // Idle with items held by a stop: the count and the way out.
    state.run_finished();
    state.queue_hold_after_stop();
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(
        shows(&terminal, "1 queued · Enter sends the next"),
        "{text}"
    );

    // Idle with an empty queue: today's hint.
    assert!(state.queue_send_first().is_some());
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "Enter send · Esc stop"), "{text}");
}

/// A queued message names its place in line, first line only.
#[test]
fn a_queued_message_shows_its_place_in_the_transcript() {
    let (mut terminal, mut state) = fresh(80, 24);
    state.queue_prompt("first line\nsecond line");
    state.queue_prompt("next");
    draw(&mut terminal, &mut state);
    let text = screen(&terminal);
    assert!(shows(&terminal, "queued 1: first line"), "{text}");
    assert!(shows(&terminal, "queued 2: next"), "{text}");
    assert!(!shows(&terminal, "second line"), "{text}");
}
