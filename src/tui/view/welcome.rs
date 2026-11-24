//! The empty transcript's welcome block: three quiet rows in the middle
//! of the area that will fill with conversation. It exists because a blank
//! rectangle is not an answer to "is this running, and what do I type" —
//! and it goes away by itself the moment there is anything to show, so it
//! never gets in the way of a session.

use ratatui::layout::Rect;
use ratatui::text::{Line as UiLine, Span};

use super::super::theme::Palette;

/// The block's rows, in order: the name, one line under it, and the keys.
const NAME: &str = "ganggang";
const SUBTITLE: &str = "a small, sharp agentic harness";
const KEYS: &str = "Enter send · / commands · Ctrl+G editor · Esc stop";

/// The welcome block for an area, centred as a block and horizontally
/// within it. Three rows, never more: an area too short for them (or too
/// narrow for the name) gets nothing rather than a truncated slogan, and a
/// row wider than the area simply clips — `Paragraph` does not wrap.
pub fn block(palette: &Palette, area: Rect) -> Vec<UiLine<'static>> {
    let narrowest = NAME.chars().count() as u16;
    if area.height < 3 || area.width < narrowest {
        return Vec::new();
    }
    let rows = vec![
        UiLine::from(Span::styled(NAME, palette.welcome_name())).centered(),
        UiLine::from(Span::styled(SUBTITLE, palette.welcome())).centered(),
        UiLine::from(Span::styled(KEYS, palette.welcome_keys())).centered(),
    ];
    // The vertical centre, with the odd row's worth of slack going below.
    let above = (area.height - rows.len() as u16) / 2;
    let mut block = Vec::with_capacity(rows.len() + above as usize);
    for _ in 0..above {
        block.push(UiLine::default());
    }
    block.extend(rows);
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::TRUECOLOR;

    #[test]
    fn the_block_is_three_rows_and_sits_in_the_middle() {
        let block = block(&TRUECOLOR, Rect::new(0, 0, 80, 20));
        assert_eq!(block.len(), 8 + 3, "air above, then the three rows");
        assert_eq!(block[8].spans[0].content, NAME);
        assert_eq!(block[8].spans[0].style.fg, Some(TRUECOLOR.accent));
        assert_eq!(block[9].spans[0].content, SUBTITLE);
        assert_eq!(block[9].spans[0].style.fg, Some(TRUECOLOR.muted));
        assert_eq!(block[10].spans[0].content, KEYS);
        assert_eq!(block[10].spans[0].style.fg, Some(TRUECOLOR.dim));
        assert_eq!(
            block[9].alignment,
            Some(ratatui::layout::Alignment::Center),
            "centred, not flush left"
        );
    }

    #[test]
    fn a_small_area_gets_nothing_rather_than_a_truncated_slogan() {
        assert!(block(&TRUECOLOR, Rect::new(0, 0, 80, 2)).is_empty());
        assert!(block(&TRUECOLOR, Rect::new(0, 0, 3, 20)).is_empty());
        assert!(block(&TRUECOLOR, Rect::new(0, 0, 0, 0)).is_empty());
        // Three rows exactly: no air, no row lost.
        assert_eq!(block(&TRUECOLOR, Rect::new(0, 0, 60, 3)).len(), 3);
    }
}
