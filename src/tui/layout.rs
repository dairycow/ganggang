//! The frame's geometry: how the three areas (transcript, composer,
//! footer) divide a terminal, and when the transcript's scrollbar earns its
//! column.
//!
//! Pure numbers in, pure numbers out — no terminal, no widgets — so the
//! rules a visual pass is most likely to get wrong (a composer that eats a
//! 24-row terminal, text hidden under a scrollbar, a footer pushed off the
//! screen) are the rules the tests pin down. The composer's own geometry
//! is in [`composer`](super::composer); this is where its height is paid for.

use ratatui::layout::Rect;

use super::composer;

/// The transcript's minimum: a terminal that short still shows something
/// of the conversation, even if the composer has to shrink to keep it.
pub const MIN_TRANSCRIPT_ROWS: u16 = 1;

/// The smallest composer that is one: two rules and a single text row.
pub const USABLE_COMPOSER: u16 = composer::BORDERS + 1;

/// A scrollbar on a terminal this narrow would leave one column of text:
/// not worth it, and the wrap would be silly.
const SCROLLBAR_MIN_WIDTH: u16 = 6;

/// The three areas of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub transcript: Rect,
    /// The composer's box, border rows included, its height already chosen
    /// for the draft (see [`composer::height`]).
    pub composer: Rect,
    pub footer: Rect,
}

impl Areas {
    /// Split `screen`: footer on the last row, composer above it with the
    /// height the draft calls for, transcript filling the rest.
    pub fn new(screen: Rect, lines: usize) -> Self {
        let footer = Rect {
            x: screen.x,
            y: screen.bottom().saturating_sub(1),
            width: screen.width,
            height: 1.min(screen.height),
        };
        // What the transcript and the composer divide between them.
        let stack = Rect {
            x: screen.x,
            y: screen.y,
            width: screen.width,
            height: screen.height.saturating_sub(footer.height),
        };
        // The transcript keeps its minimum first: a three-row terminal
        // leaves the composer one text row, not none.
        let mut room = stack.height.saturating_sub(MIN_TRANSCRIPT_ROWS);
        // The one exception: a box needs three rows to show a single typed
        // line (two rules and a text row). A composer of two rules shows
        // nothing typed, which is no composer at all — so wherever the
        // stack can hold a usable box, the transcript gives up its row
        // first.
        if stack.height >= USABLE_COMPOSER {
            room = room.max(USABLE_COMPOSER);
        }
        let height = composer::height(lines, room);
        let transcript_height = stack.height.saturating_sub(height);
        Self {
            transcript: Rect {
                x: stack.x,
                y: stack.y,
                width: stack.width,
                height: transcript_height,
            },
            composer: Rect {
                x: stack.x,
                y: stack.y + transcript_height,
                width: stack.width,
                height,
            },
            footer,
        }
    }
}

/// Whether the transcript's scrollbar has anything to say: only when the
/// content does not fit the view. Content that fits shows no chrome at all,
/// which is the whole point of a conditional scrollbar.
///
/// One hidden row is enough. The alternative — waiting for a PageUp — would
/// slide the text sideways the moment the bar appeared; deciding on the
/// content keeps the layout predictable.
pub fn show_scrollbar(content_rows: u16, view_rows: u16, transcript: Rect) -> bool {
    content_rows > view_rows && transcript.width >= SCROLLBAR_MIN_WIDTH && transcript.height >= 2
}

/// The transcript's text area: full width, or one column narrower once the
/// scrollbar has its column. Wrapping uses this, so a row is never drawn
/// under the thumb.
pub fn transcript_text(transcript: Rect, scrollbar: bool) -> Rect {
    if !scrollbar || transcript.width == 0 {
        return transcript;
    }
    Rect {
        width: transcript.width - 1,
        ..transcript
    }
}

/// The scrollbar's column: the transcript's rightmost cells.
pub fn scrollbar_area(transcript: Rect) -> Rect {
    Rect {
        x: transcript.right().saturating_sub(1),
        y: transcript.y,
        width: 1,
        height: transcript.height,
    }
}

/// What the scrollbar is told about a transcript: how much there is, which
/// row is first on screen, and how many rows the screen holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumb {
    /// The track stands for this many rows: one per scroll position, plus
    /// the view.
    pub content: usize,
    /// The first visible row — 0 when the top of the transcript is on
    /// screen, the last scroll position when the newest row is.
    pub position: usize,
    /// Rows on screen.
    pub viewport: usize,
}

/// The scrollbar's figures for `total` wrapped rows in a view `view` rows
/// tall, scrolled `offset` rows up from the bottom.
///
/// The thumb should be the view's share of the content (`view / total` of
/// the track) and should run the whole track: at the bottom while the
/// newest row shows, at the top once the first row does. The scrollbar
/// widget places and sizes its thumb as fractions of
/// `content - 1 + viewport`, so the track is said to stand for exactly
/// `total` rows — one per scroll position (`total - view`) plus the view —
/// and `position` is the first visible row. Any other pair leaves the
/// thumb short of one end: told the row count and the view, it never
/// reaches the bottom of a mostly-full transcript.
pub fn thumb(total: u16, view: u16, offset: u16) -> Thumb {
    let scrollable = total.saturating_sub(view);
    let offset = offset.min(scrollable);
    Thumb {
        content: scrollable as usize + 1,
        position: (scrollable - offset) as usize,
        viewport: view as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn the_split_puts_the_footer_last_and_gives_the_rest_to_the_transcript() {
        let areas = Areas::new(screen(80, 24), 0);
        assert_eq!(areas.footer, Rect::new(0, 23, 80, 1));
        assert_eq!(areas.composer, Rect::new(0, 20, 80, 3));
        assert_eq!(areas.transcript, Rect::new(0, 0, 80, 20));

        let areas = Areas::new(screen(80, 24), 6);
        assert_eq!(areas.composer, Rect::new(0, 15, 80, 8));
        assert_eq!(areas.transcript, Rect::new(0, 0, 80, 15));
        // The three areas tile the screen exactly, at every size tried.
        for (w, h) in [(80, 24), (40, 12), (20, 4), (60, 3)] {
            for lines in [0, 3, 20] {
                let areas = Areas::new(screen(w, h), lines);
                assert_eq!(
                    areas.transcript.height + areas.composer.height + areas.footer.height,
                    h,
                    "{w}×{h}, {lines} lines: {areas:?}"
                );
                assert_eq!(areas.transcript.bottom(), areas.composer.y);
                assert_eq!(areas.composer.bottom(), areas.footer.y);
            }
        }
    }

    /// A composer that grows without limit is how a tall draft eats the
    /// conversation: the transcript keeps its row.
    #[test]
    fn a_short_terminal_shrinks_the_composer_rather_than_the_transcript() {
        let areas = Areas::new(screen(40, 3), 30);
        assert_eq!(areas.footer.height, 1);
        assert_eq!(areas.transcript.height, 1);
        assert_eq!(areas.composer.height, 1);

        // One row more, and the composer becomes usable before the
        // transcript keeps a row: two rules with nothing between them show
        // nothing typed, whatever the draft.
        let areas = Areas::new(screen(40, 4), 30);
        assert_eq!(areas.footer.height, 1);
        assert_eq!(areas.composer, Rect::new(0, 0, 40, 3), "one text row");
        assert_eq!(areas.transcript.height, 0);
        let areas = Areas::new(screen(40, 5), 30);
        assert_eq!(areas.transcript.height, 1, "the transcript's row is back");
        assert_eq!(areas.composer.height, 3);

        // Even a one-row terminal divides without panicking.
        let areas = Areas::new(screen(20, 1), 0);
        assert_eq!(areas.footer, Rect::new(0, 0, 20, 1));
        assert_eq!(areas.transcript.height, 0);
        assert_eq!(areas.composer.height, 0);
        // A zero-size terminal: three empty areas, no panic.
        let areas = Areas::new(Rect::ZERO, 4);
        assert_eq!(areas.transcript, Rect::ZERO);
    }

    #[test]
    fn the_scrollbar_appears_only_when_the_content_overflows() {
        let transcript = screen(80, 20);
        assert!(!show_scrollbar(20, 20, transcript), "it fits: no chrome");
        assert!(!show_scrollbar(5, 20, transcript), "less so");
        assert!(
            show_scrollbar(21, 20, transcript),
            "one hidden row is worth a bar"
        );
        // A bar on a narrow terminal costs more than it tells.
        assert!(
            !show_scrollbar(99, 20, Rect::new(0, 0, 5, 20)),
            "one column of text is not a transcript"
        );
        assert!(
            !show_scrollbar(99, 20, Rect::new(0, 0, 80, 1)),
            "no room for a thumb"
        );
    }

    #[test]
    fn reserving_the_column_narrows_the_text_not_the_view() {
        let transcript = Rect::new(0, 0, 80, 20);
        assert_eq!(transcript_text(transcript, false), transcript);
        assert_eq!(
            transcript_text(transcript, true),
            Rect::new(0, 0, 79, 20),
            "text wraps to the bar, never under it"
        );
        assert_eq!(scrollbar_area(transcript), Rect::new(79, 0, 1, 20));
        // A one-column transcript keeps the width it has.
        assert_eq!(transcript_text(Rect::new(0, 0, 1, 5), true).width, 0);
    }

    /// The thumb is the view's share of the track and runs all of it: it
    /// starts at the bottom (the newest row), reaches the top with the
    /// first row, and sits in between in between.
    #[test]
    fn the_thumb_runs_the_whole_track_from_the_newest_row_to_the_first() {
        // Twenty-five rows in a twenty-row view: five rows to scroll.
        let at_rest = thumb(25, 20, 0);
        assert_eq!(at_rest.position, 5, "the last scroll position: the bottom");
        assert_eq!(at_rest.content, 6, "five scroll positions and the view");
        assert_eq!(at_rest.viewport, 20);
        assert_eq!(
            thumb(25, 20, 5).position,
            0,
            "scrolled to the first row: the top"
        );
        assert_eq!(thumb(25, 20, 2).position, 3);
        assert_eq!(thumb(25, 20, 99).position, 0, "past the top is the top");

        // Where the widget puts it, by its own rule: a thumb of
        // `view / total` track rows, starting `position / total` of the
        // way down (`total` being `content - 1 + viewport`).
        let place = |t: Thumb, track: usize| {
            let total = t.content - 1 + t.viewport;
            let length = (t.viewport * track + total / 2) / total;
            ((t.position * track + total / 2) / total, length)
        };
        assert_eq!(place(at_rest, 20), (4, 16), "ends on the track's last row");
        assert_eq!(place(thumb(25, 20, 5), 20), (0, 16), "starts on the first");
        assert_eq!(
            place(thumb(100, 20, 0), 20),
            (16, 4),
            "ends on the last row"
        );
        assert_eq!(place(thumb(100, 20, 80), 20), (0, 4), "starts on the first");

        // Content that fits has nothing to scroll.
        assert_eq!(
            thumb(5, 20, 0),
            Thumb {
                content: 1,
                position: 0,
                viewport: 20
            }
        );
    }
}
