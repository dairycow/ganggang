//! The mark's beat: the crest's reds blended into one smooth pulse.
//!
//! At rest the footer's `gg` sits in the crest's scarlet. While a run
//! works, the same name flares and sinks: scarlet out to the cheek's
//! orange-red, up to a bright peak half way toward the beak's cream,
//! back down the way it came, on down to the deep crest red, and home —
//! one shade per tick, forty-eight shades a breath, every shade a blend
//! of two colours the theme already has. The pulse invents no colour:
//! its stops are the crest's reds and the cream, and what lies between
//! is only ever the mix of two neighbours.

use ratatui::style::Color;

use super::Palette;
use super::fallback::{nearest_index, rgb_of_index};

/// Frames in one breath of the beat: forty-eight shades a cycle, eight
/// per leg, so at [`crate::tui::tick::PERIOD`] a frame the whole breath
/// takes 1.2 s and no single step is large enough to see.
pub const BEAT_FRAMES: usize = 48;

/// The legs the pulse walks: scarlet to cheek, cheek up to the peak,
/// peak back to cheek, cheek home to scarlet, scarlet down to the deep
/// red, deep red home again. The path is closed, the last stop the
/// first, so the wrap is seamless.
const LEGS: usize = 6;

/// How far the peak leans from the scarlet toward the cream, as a
/// fraction (`numerator`, `denominator`): half way.
const PEAK_LEAN: (usize, usize) = (1, 2);

/// Frames per leg.
const STEPS: usize = BEAT_FRAMES / LEGS;

const _: () = assert!(
    BEAT_FRAMES.is_multiple_of(LEGS),
    "the beat must blend in equal steps"
);

impl Palette {
    /// The mark's colour on `frame` of the beat: exact on the colour each
    /// leg starts from, a blend of its leg's two ends elsewhere. A
    /// 256-colour palette blends in RGB and rounds each shade to the
    /// nearest cube or ramp entry, so two neighbouring shades may land on
    /// one index; truecolour keeps every shade distinct. A colour the
    /// beat cannot read (one with no RGB behind it) holds the mark at the
    /// scarlet.
    pub fn crest_beat(&self, frame: usize) -> Color {
        let scarlet = rgb_of(self.accent);
        let cheek = rgb_of(self.accent_soft);
        let deep = rgb_of(self.accent_deep);
        let cream = rgb_of(self.text);
        let (Some(scarlet), Some(cheek), Some(deep), Some(cream)) = (scarlet, cheek, deep, cream)
        else {
            return self.accent;
        };
        let peak = blend(scarlet, cream, PEAK_LEAN.0, PEAK_LEAN.1);
        let path = [scarlet, cheek, peak, cheek, scarlet, deep, scarlet];
        let step = frame % BEAT_FRAMES;
        let (leg, t) = (step / STEPS, step % STEPS);
        let (r, g, b) = blend(path[leg], path[leg + 1], t, STEPS);
        match self.accent_deep {
            Color::Indexed(_) => Color::Indexed(nearest_index(r, g, b)),
            _ => Color::Rgb(r, g, b),
        }
    }
}

/// `from` moved `num`/`den` of the way to `to`, channel by channel, in
/// whole units. Integer on purpose: deterministic, and at eight steps a
/// leg the rounding is a unit or two of a channel.
fn blend(from: (u8, u8, u8), to: (u8, u8, u8), num: usize, den: usize) -> (u8, u8, u8) {
    let mix = |a: u8, b: u8| {
        let (a, b) = (i16::from(a), i16::from(b));
        (a + (b - a) * num as i16 / den as i16).clamp(0, 255) as u8
    };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// The RGB triple behind a palette colour: truecolour as it stands, an
/// xterm-256 index by its cube or ramp value. The terminal's base
/// sixteen have no fixed RGB here, so they read as unreadable.
fn rgb_of(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index) => rgb_of_index(index),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::TRUECOLOR as T;
    use crate::tui::theme::indexed_palette;

    /// The designed peak: the scarlet (217, 75, 75) half way to the
    /// cream (233, 227, 215).
    const PEAK: Color = Color::Rgb(225, 151, 145);

    /// The stops land exactly where the path says: scarlet at the start
    /// (the crest frame), the cheek a sixth round, the peak a third
    /// round, the cheek again at the half, scarlet two thirds round, the
    /// deep red five sixths round, scarlet again at the wrap.
    #[test]
    fn the_beat_lands_on_its_stops_exactly() {
        let stops = [
            (0, T.accent),
            (BEAT_FRAMES / 6, T.accent_soft),
            (BEAT_FRAMES / 3, PEAK),
            (BEAT_FRAMES / 2, T.accent_soft),
            (2 * BEAT_FRAMES / 3, T.accent),
            (5 * BEAT_FRAMES / 6, T.accent_deep),
            (BEAT_FRAMES, T.accent),
        ];
        for (frame, colour) in stops {
            assert_eq!(T.crest_beat(frame), colour, "frame {frame}");
        }
    }

    /// The flare is a real one: the peak is lighter than anything the
    /// mark wears at rest and the deep end darker than the scarlet, so
    /// a breath is seen as well as timed.
    #[test]
    fn the_peak_is_lighter_than_the_crest_and_the_deep_end_darker() {
        let lightness = |color: Color| match color {
            Color::Rgb(r, g, b) => u16::from(r) + u16::from(g) + u16::from(b),
            other => panic!("{other:?} is not rgb"),
        };
        assert!(lightness(PEAK) > lightness(T.accent_soft));
        assert!(lightness(T.accent_deep) < lightness(T.accent));
    }

    /// Between the stops the beat blends: each frame lies within its
    /// leg's two ends in every channel and off both ends as a shade, and
    /// no frame repeats its neighbour's shade, so the pulse reads as one
    /// breath.
    #[test]
    fn the_blend_walks_each_leg_without_jumping() {
        let rgb = |color: Color| match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("{other:?} is not rgb"),
        };
        let legs = [
            (T.accent, T.accent_soft),
            (T.accent_soft, PEAK),
            (PEAK, T.accent_soft),
            (T.accent_soft, T.accent),
            (T.accent, T.accent_deep),
            (T.accent_deep, T.accent),
        ];
        for (leg, (from, to)) in legs.iter().enumerate() {
            let (fr, fg, fb) = rgb(*from);
            let (tr, tg, tb) = rgb(*to);
            for step in 1..STEPS {
                let shade @ (r, g, b) = rgb(T.crest_beat(leg * STEPS + step));
                // A channel only a unit or two wide (red, cheek to peak)
                // holds its value for a step, so each channel is held to
                // its range; the shade as a whole must be off both ends.
                let within = |x: u8, lo: u8, hi: u8| (lo.min(hi)..=lo.max(hi)).contains(&x);
                assert!(
                    within(r, fr, tr) && within(g, fg, tg) && within(b, fb, tb),
                    "leg {leg} step {step}: {shade:?} is outside the range of \
                     ({fr}, {fg}, {fb}) and ({tr}, {tg}, {tb})"
                );
                assert_ne!(
                    shade,
                    (fr, fg, fb),
                    "leg {leg} step {step} is still on the start"
                );
                assert_ne!(
                    shade,
                    (tr, tg, tb),
                    "leg {leg} step {step} has already reached the end"
                );
            }
        }
        let shades: Vec<(u8, u8, u8)> = (0..BEAT_FRAMES)
            .map(|frame| rgb(T.crest_beat(frame)))
            .collect();
        for pair in shades.windows(2) {
            assert_ne!(pair[0], pair[1], "two frames share a shade: {shades:?}");
        }
        let wrapped = rgb(T.crest_beat(BEAT_FRAMES));
        assert_ne!(Some(wrapped), shades.last().copied(), "the wrap moves");
    }

    /// The indexed palette beats in cube entries, its stops are its own
    /// colours, and the peak keeps an entry of its own.
    #[test]
    fn the_indexed_palette_beats_in_cube_entries() {
        let p = indexed_palette();
        for frame in 0..BEAT_FRAMES {
            assert!(
                matches!(p.crest_beat(frame), Color::Indexed(_)),
                "frame {frame} is not an index"
            );
        }
        assert_eq!(p.crest_beat(0), p.accent, "the crest frame round-trips");
        assert_eq!(
            p.crest_beat(BEAT_FRAMES / 6),
            p.accent_soft,
            "the cheek round-trips"
        );
        assert_eq!(
            p.crest_beat(5 * BEAT_FRAMES / 6),
            p.accent_deep,
            "the deep end round-trips"
        );
        let peak = p.crest_beat(BEAT_FRAMES / 3);
        assert_ne!(peak, p.accent, "the peak is not the scarlet");
        assert_ne!(peak, p.accent_soft, "the peak is not the cheek");
        assert_ne!(peak, p.accent_deep, "the peak is not the deep red");
    }

    /// A colour with no RGB behind it cannot blend: the mark holds the
    /// scarlet rather than guess.
    #[test]
    fn a_palette_the_beat_cannot_read_holds_the_scarlet() {
        let mut p = T;
        p.accent = Color::Red;
        assert_eq!(p.crest_beat(3), Color::Red);
        let mut p = T;
        p.text = Color::White;
        assert_eq!(p.crest_beat(BEAT_FRAMES / 3), T.accent);
    }
}
