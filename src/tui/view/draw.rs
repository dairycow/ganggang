//! Drawing one frame: the theme's ground, the transcript (wrapped, bottom-
//! anchored, scrollbar when it overflows, welcome block while it is empty),
//! the composer, the footer, and, floating above the composer, the
//! suggestion popup or the `/resume` picker.
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
use super::super::layout::{Areas, scrollbar_area, show_scrollbar, thumb, transcript_text};
use super::super::line::{styled_line, wrap_line};
use super::super::table;
use super::super::theme::Palette;
use super::super::transcript::{RenderedLine, TableRow};
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
    // The two popups share one patch of screen, and the picker has the
    // composer while it is open: it is the one that draws.
    if state.picker_open() {
        draw_picker(frame, state, areas);
    } else {
        draw_popup(frame, state, areas);
    }
}

/// The transcript's rows at a known width. A run of table rows is laid
/// out as one table — columns need their neighbours' widths — and every
/// other line wraps on its own.
fn wrapped(lines: &[RenderedLine], width: usize) -> Vec<RenderedLine> {
    let mut rows = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].table.is_none() {
            rows.extend(wrap_line(&lines[i], width));
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].table.is_some() {
            if i > start && lines[i].table.as_ref().is_some_and(|row| row.header) {
                break;
            }
            i += 1;
        }
        let table: Vec<TableRow> = lines[start..i]
            .iter()
            .filter_map(|line| line.table.clone())
            .collect();
        for row in table::layout(&table, width) {
            rows.extend(wrap_line(&row, width));
        }
    }
    rows
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
    // The first visible row: the bottom of the content at rest, higher up
    // as the reader pages back.
    let top = total.saturating_sub(state.offset_from_bottom + text.height);
    // Published with the row count: the scroll clamp stops where the first
    // row reaches the top of the view.
    state.last_view_rows = text.height;
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

    if bar && total > text.height {
        let (thumb_style, track) = palette.scrollbar();
        // The thumb points at the first row on screen and is the view's
        // share of the track; why it is told these three figures (and not
        // the row count) is [`thumb`]'s to explain.
        let figures = thumb(total, text.height, state.offset_from_bottom);
        let mut scroll = ScrollbarState::new(figures.content)
            .position(figures.position)
            .viewport_content_length(figures.viewport);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(thumb_style)
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

/// The `/resume` picker: the same floating panel as the suggestion popup,
/// over the transcript and directly above the composer, with one row per
/// session the composer's filter keeps. The panel shows a window of the
/// rows and the window follows the highlight, so a directory with a
/// hundred sessions is scrolled rather than cut to what fits. A scope
/// with no row at all — an empty directory, or a filter that keeps no
/// row — draws a two-row panel that says why in its own ground: the
/// panel floats over the transcript's newest rows, so the transcript
/// cannot say it there. The panel's last row is the hint, or, after a
/// failed open, the error in its place.
fn draw_picker(frame: &mut Frame, state: &mut State, areas: Areas) {
    state.sync_resume();
    let rows = state.picker_matches();
    let width = areas.composer.width as usize;
    if rows.is_empty() {
        // The picker is open but its scope holds no row: one row of
        // explanation over the hint, not no panel.
        let height = 2.min(areas.composer.y);
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
        frame.render_widget(Fill::new(" ").style(state.palette.popup()), area);
        let last = picker_last_row(state, &state.palette, width, None);
        let panel = vec![
            crate::tui::picker::empty_row(
                &state.palette,
                state.session_scope_empty(),
                state.session_scope_all(),
                state.editor.text(),
                width,
            ),
            last,
        ];
        frame.render_widget(Paragraph::new(panel), area);
        return;
    }
    let palette = state.palette;
    let height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1)
        .min(areas.composer.y);
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
    let shown = (height as usize - 1).min(rows.len());
    let selected = state
        .picker
        .as_ref()
        .map_or(0, |pick| pick.selected)
        .min(rows.len() - 1);
    let start = composer::scrolled_row(selected, shown, rows.len());
    let mut panel = Vec::with_capacity(height as usize);
    for (i, row) in rows.iter().enumerate().skip(start).take(shown) {
        panel.push(super::super::picker::row_line(
            &palette,
            row,
            i == selected,
            area.width as usize,
        ));
    }
    let at = (rows.len() > shown).then_some((selected, rows.len()));
    panel.push(picker_last_row(state, &palette, area.width as usize, at));
    frame.render_widget(Paragraph::new(panel), area);
}

/// The picker panel's last row: the error of a failed open while the
/// picker holds one, in place of the hint; otherwise the hint, with its
/// place counter when the list is longer than the panel.
fn picker_last_row(
    state: &State,
    palette: &Palette,
    width: usize,
    at: Option<(usize, usize)>,
) -> UiLine<'static> {
    state
        .picker
        .as_ref()
        .and_then(|pick| pick.error.as_deref())
        .map(|error| crate::tui::picker::error_row(palette, error, width))
        .unwrap_or_else(|| {
            crate::tui::picker::hint_row(palette, width, state.session_scope_all(), at)
        })
}

#[cfg(test)]
mod tests {
    use super::super::super::transcript::{Align, LineKind};
    use super::*;

    fn table_row(cells: &[&str], header: bool) -> RenderedLine {
        RenderedLine::new("", LineKind::Table).with_table(TableRow {
            cells: cells.iter().map(|cell| cell.to_string()).collect(),
            aligns: vec![Align::Left, Align::Left],
            header,
        })
    }

    fn texts(lines: &[RenderedLine]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    #[test]
    fn a_run_of_table_rows_is_laid_out_once_at_the_draw_width() {
        let lines = vec![
            RenderedLine::new("intro", LineKind::Assistant),
            table_row(&["name", "kind"], true),
            table_row(&["a", "b"], false),
            RenderedLine::new("after", LineKind::Assistant),
        ];
        let rows = wrapped(&lines, 40);
        assert_eq!(rows[0].text, "intro");
        assert_eq!(rows[0].kind, LineKind::Assistant);
        assert_eq!(rows[1].text, "name │ kind");
        assert!(rows[2].text.contains('┼'), "a rule under the header");
        assert_eq!(rows[3].text, "a    │ b   ");
        assert!(rows[1..4].iter().all(|row| row.kind == LineKind::Table));
        assert_eq!(rows[4].text, "after");

        let narrow = wrapped(&lines, 10);
        assert_eq!(
            texts(&narrow),
            vec!["intro", "• a", "  kind: b", "after"],
            "a table that does not fit becomes a list"
        );
    }
}
