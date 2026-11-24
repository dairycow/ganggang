//! Drawing one frame: the theme's ground, the transcript (wrapped, bottom-
//! anchored, scrollbar when it overflows, welcome block while it is empty),
//! the composer, the footer, and the suggestion popup floating above the
//! composer.
//!
//! State is read, never written, except for two figures the loop needs
//! back: the wrapped-row count that clamps scrolling, and the number of
//! popup rows actually drawn. Geometry comes from
//! [`layout`](crate::tui::layout) and [`composer`](crate::tui::composer);
//! colour comes from [`theme`](crate::tui::theme). Nothing here names a
//! colour.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line as UiLine;
use ratatui::widgets::{Clear, Fill, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::super::composer;
use super::super::layout::{Areas, scrollbar_area, show_scrollbar, transcript_text};
use super::super::line::{styled_line, wrap_line};
use super::super::transcript::RenderedLine;
use super::State;
use super::welcome;

/// Draw one frame. The theme owns the screen: every cell is painted
/// `bg`/`text` before anything else is drawn, so what a reader sees is
/// gg's charcoal rather than whatever the terminal brings.
///
/// Takes `&mut State` to publish the wrapped-row count the scroll clamp
/// needs and the popup's drawn-row count.
pub fn render(frame: &mut Frame, state: &mut State) {
    let area = frame.area();
    let palette = state.palette();
    // The ground under everything: every cell, not only the ones a widget
    // happens to write to.
    frame.render_widget(Fill::new(" ").style(palette.frame()), area);

    // The composer's height follows the draft, so the transcript takes what
    // is left of the terminal.
    let areas = Areas::new(area, state.editor.line_count());
    let lines = state.lines();

    // Wrap at the transcript's full width and see whether that overflows.
    // If it does, the scrollbar takes its column and the text re-wraps one
    // column narrower — self-consistent either way, because rows that fit
    // at full width have nothing to scroll and so no column to make room
    // for. (One row wrapping differently at the narrower width cannot add a
    // row the first pass did not already need: a narrower wrap only ever
    // splits more.)
    let rows = wrapped(&lines, areas.transcript.width as usize);
    let bar = show_scrollbar(rows.len() as u16, areas.transcript.height, areas.transcript);
    let rows = if bar {
        wrapped(
            &lines,
            transcript_text(areas.transcript, true).width as usize,
        )
    } else {
        rows
    };
    state.last_row_count = rows.len() as u16;

    draw_transcript(frame, state, areas, &rows, bar);
    composer::draw(
        frame,
        &palette,
        &state.editor,
        areas.composer,
        state.busy,
        state.stopping(),
    );
    frame.render_widget(
        Paragraph::new(state.footer.spans(&palette)).style(Style::new().bg(palette.surface)),
        areas.footer,
    );
    draw_popup(frame, state, areas);
}

/// The transcript's rows at a known width.
fn wrapped(lines: &[RenderedLine], width: usize) -> Vec<RenderedLine> {
    lines.iter().flat_map(|l| wrap_line(l, width)).collect()
}

/// The transcript: wrapped rows, bottom-anchored, a scrollbar in the
/// column held for it when the content overflows, and — while there is
/// nothing to show — the welcome block instead of a blank area.
fn draw_transcript(
    frame: &mut Frame,
    state: &mut State,
    areas: Areas,
    rows: &[RenderedLine],
    bar: bool,
) {
    let palette = state.palette;
    let text = transcript_text(areas.transcript, bar);
    let width = text.width as usize;
    let total = rows.len() as u16;
    let top = total.saturating_sub(state.offset_from_bottom + text.height);
    let shown: Vec<UiLine<'static>> = rows
        .iter()
        .skip(top as usize)
        .take(text.height as usize)
        .map(|row| styled_line(&palette, row, Some(width)))
        .collect();
    frame.render_widget(Paragraph::new(shown), text);

    // The welcome block answers "is this running, and what do I type" for
    // a session with nothing in it yet — not for a transcript whose only
    // block happens to render no lines (hidden thinking, say).
    if state.is_quiet() {
        let block = welcome::block(&palette, text);
        if !block.is_empty() {
            frame.render_widget(Paragraph::new(block), text);
        }
    }

    if bar {
        let (thumb, track) = palette.scrollbar();
        // The thumb's position is the first visible row, which is what a
        // reader expects a thumb to point at: the bottom of the content at
        // rest, higher up as they page back.
        let position = total.saturating_sub(state.offset_from_bottom + 1) as usize;
        let mut scroll = ScrollbarState::new(total as usize)
            .position(position)
            .viewport_content_length(areas.transcript.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(thumb)
                .track_style(track)
                .begin_symbol(None)
                .end_symbol(None),
            scrollbar_area(areas.transcript),
            &mut scroll,
        );
    }
}

/// Slash-command suggestions: a floating list over the transcript,
/// directly above the composer, while the composer holds a bare command
/// word. Clear wipes the rows underneath, then the popup's own raised
/// surface fills the area, so it reads as one panel rather than text
/// dropped over the conversation. The height caps to the room above the
/// composer: a tiny terminal simply shows fewer rows, and publishes how
/// many it drew so the selection can never walk past the screen.
fn draw_popup(frame: &mut Frame, state: &mut State, areas: Areas) {
    state.sync_suggest();
    let list = state.suggestions();
    state.suggest_view_rows = 0;
    if list.is_empty() {
        return;
    }
    let palette = state.palette;
    let height = (list.len() as u16 + 1).min(areas.composer.y);
    if height < 2 {
        return;
    }
    let area = Rect {
        x: areas.composer.x,
        y: areas.composer.y - height,
        width: areas.composer.width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Fill::new(" ").style(palette.popup()), area);
    let shown = (height as usize - 1).min(list.len());
    state.suggest_view_rows = shown;
    let selected = state.suggest_selected.min(shown - 1);
    let mut popup = Vec::with_capacity(height as usize);
    for (i, row) in list.iter().take(shown).enumerate() {
        popup.push(super::super::suggest::suggestion_row(
            &palette,
            row,
            i == selected,
            area.width as usize,
        ));
    }
    popup.push(super::super::suggest::hint_row(
        &palette,
        area.width as usize,
    ));
    frame.render_widget(Paragraph::new(popup), area);
}
