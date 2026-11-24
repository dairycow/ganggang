//! The 256-colour half of the theme: what each role's colour becomes when
//! the terminal will not take a truecolor value.
//!
//! Nearest-neighbour over the 6×6×6 cube and the grey ramp gets most roles
//! right, but fifteen roles cannot all have their own nearest colour: the
//! two reds (`accent`, `error`) and the two dark neutrals (`surface_alt`,
//! `selection`) collapse onto a single index each. So the roles claim
//! colours in the order their colour carries meaning — marks and status
//! first, then text and its quieter greys, then the ground — and a role
//! whose nearest entry is already taken steps to the next nearest free one.
//! Deterministic, no hand-written table, and fifteen distinct colours by
//! construction: what a 256-colour terminal can honestly give a fifteen-role
//! palette.

use ratatui::style::Color;

use super::Palette;

/// The designed palette with every role replaced by an xterm-256 entry.
pub fn indexed_palette() -> Palette {
    let mut out = super::TRUECOLOR;
    let mut taken = [false; 256];
    let mut claim = |color: &mut Color| {
        let Color::Rgb(r, g, b) = *color else {
            return;
        };
        // The nearest entry — unless a role whose colour carries more
        // meaning has already claimed it, in which case the next nearest
        // entry still free.
        let nearest = nearest_index(r, g, b);
        let choice = if taken[usize::from(nearest)] {
            nearest_indexes(r, g, b).find(|index| !taken[usize::from(*index)])
        } else {
            Some(nearest)
        };
        if let Some(index) = choice {
            taken[usize::from(index)] = true;
            *color = Color::Indexed(index);
        }
    };
    for color in [
        &mut out.accent,
        &mut out.accent_soft,
        &mut out.accent_deep,
        &mut out.error,
        &mut out.warn,
        &mut out.ok,
        &mut out.text,
        &mut out.code,
        &mut out.muted,
        &mut out.dim,
        &mut out.border,
        &mut out.surface,
        &mut out.surface_alt,
        &mut out.selection,
        &mut out.bg,
    ] {
        claim(color);
    }
    out
}

/// The xterm-256 colour cube's steps.
const CUBE: [u16; 6] = [0, 95, 135, 175, 215, 255];

/// The xterm-256 index nearest an RGB triple: the best of the 6×6×6 colour
/// cube (16..=231) and the grey ramp (232..=255), by squared distance.
/// Deterministic, table-free, dependency-free.
pub fn nearest_index(r: u8, g: u8, b: u8) -> u8 {
    nearest_indexes(r, g, b).next().unwrap_or(0)
}

/// Every xterm-256 candidate for an RGB triple, nearest first: what the
/// palette walks when its first choice has been claimed by a role that
/// needed it more.
fn nearest_indexes(r: u8, g: u8, b: u8) -> impl Iterator<Item = u8> {
    let (r, g, b) = (u16::from(r), u16::from(g), u16::from(b));
    let side = |a: u16, other: u16| -> u32 {
        let d = u32::from(a.max(other)) - u32::from(a.min(other));
        d * d
    };
    let mut candidates: Vec<(u32, u8)> = Vec::with_capacity(240);
    for ri in 0..6u8 {
        for gi in 0..6u8 {
            for bi in 0..6u8 {
                let d = side(CUBE[ri as usize], r)
                    + side(CUBE[gi as usize], g)
                    + side(CUBE[bi as usize], b);
                candidates.push((d, 16 + 36 * ri + 6 * gi + bi));
            }
        }
    }
    for step in 0..24u16 {
        let grey = 8 + 10 * step;
        candidates.push((
            side(grey, r) + side(grey, g) + side(grey, b),
            232 + step as u8,
        ));
    }
    // Nearest first, index order on a tie: the choice is reproducible.
    candidates.sort_by_key(|(d, index)| (*d, u16::from(*index)));
    candidates.into_iter().map(|(_, index)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Palette, TRUECOLOR};

    /// The grey a fallback colour reads as — for the test that checks the
    /// design's ordering survived the collapse, not the exact hue.
    fn lightness(color: Color) -> u16 {
        let Color::Indexed(index) = color else {
            unreachable!("the fallback palette is indexed");
        };
        if (232..=255).contains(&index) {
            return 8 + 10 * (index - 232) as u16;
        }
        let i = index - 16;
        let (r, g, b) = (
            CUBE[(i / 36) as usize],
            CUBE[((i / 6) % 6) as usize],
            CUBE[(i % 6) as usize],
        );
        (r + g + b) / 3
    }

    #[test]
    fn every_role_survives_as_an_indexed_colour() {
        let p = indexed_palette();
        for c in [
            p.bg,
            p.surface,
            p.surface_alt,
            p.border,
            p.text,
            p.muted,
            p.dim,
            p.accent,
            p.accent_soft,
            p.accent_deep,
            p.ok,
            p.warn,
            p.error,
            p.code,
            p.selection,
        ] {
            assert!(matches!(c, Color::Indexed(_)), "{c:?} is not indexed");
        }
        assert_ne!(p.text, p.muted);
        assert_ne!(p.muted, p.dim);
        assert_ne!(p.accent, p.error);
        assert_ne!(p.accent_soft, p.accent);
        assert_ne!(p.bg, p.surface);
    }

    /// Fifteen roles, fifteen colours. Where two of them sit nearest the same
    /// entry (`accent`/`error`, `surface_alt`/`selection`) the one that carries
    /// less meaning steps aside, so no two roles ever draw the same thing.
    #[test]
    fn every_role_keeps_a_colour_of_its_own() {
        let p = indexed_palette();
        let colors = [
            p.bg,
            p.surface,
            p.surface_alt,
            p.border,
            p.text,
            p.muted,
            p.dim,
            p.accent,
            p.accent_soft,
            p.accent_deep,
            p.ok,
            p.warn,
            p.error,
            p.code,
            p.selection,
        ];
        for (i, a) in colors.iter().enumerate() {
            for b in &colors[i + 1..] {
                assert_ne!(a, b, "two fallback roles share {a:?}");
            }
        }
    }

    /// The nearest-index rule, where cube and ramp meet: a colour in the cube
    /// maps to itself, a dark neutral falls in the grey ramp, and a scarlet
    /// lands on the red side of the cube.
    #[test]
    fn nearest_index_picks_the_cube_and_the_grey_ramp_as_appropriate() {
        // An exact cube entry maps to itself: rgb(135,175,95) is 107.
        assert_eq!(nearest_index(135, 175, 95), 107);
        // An exact grey-ramp entry maps to itself: rgb(118,118,118) is 243.
        assert_eq!(nearest_index(118, 118, 118), 243);
        // The palette's own marks, as the design reads on 256 colours.
        assert_eq!(nearest_index(0xD9, 0x4B, 0x4B), 167, "scarlet crest");
        assert_eq!(nearest_index(0x16, 0x19, 0x1C), 234, "charcoal plumage");
        assert_eq!(nearest_index(0xE9, 0xE3, 0xD7), 254, "warm cream beak");
        assert_eq!(nearest_index(0x00, 0x00, 0x00), 16, "the cube's black");
        assert_eq!(nearest_index(0xFF, 0xFF, 0xFF), 231, "the cube's white");
    }

    /// The fallback is the same design, not a different one: which ground is
    /// darker and which grey sits behind which survives the collapse.
    #[test]
    fn the_fallback_keeps_the_designs_ordering() {
        let p = indexed_palette();
        assert!(lightness(p.bg) < lightness(p.surface), "the ground rises");
        assert!(lightness(p.surface) < lightness(p.surface_alt));
        assert!(lightness(p.muted) < lightness(p.text), "body text leads");
        assert!(lightness(p.dim) < lightness(p.muted), "quiet grey trails");
    }

    #[test]
    fn truecolor_is_the_default_palette() {
        assert_eq!(Palette::default(), TRUECOLOR);
    }
}
