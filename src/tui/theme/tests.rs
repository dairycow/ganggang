//! Tests for [`super`]: which terminal claims 24-bit colour, and the roles a palette exposes.

use super::*;

#[test]
fn truecolor_is_requested_only_by_a_terminal_that_claims_it() {
    for value in ["truecolor", "24bit", "TRUECOLOR", "24BIT"] {
        assert!(
            truecolor_requested(Some(value), Some("xterm")),
            "{value} claims 24-bit"
        );
    }
    // The `*-direct` family advertises direct colour in TERM itself.
    for term in ["xterm-256color-direct", "screen-256color-DIRECT"] {
        assert!(
            truecolor_requested(None, Some(term)),
            "{term} claims direct colour"
        );
    }
    // Claims gg must not assume: 256 colours is not 24-bit, and an
    // empty or missing environment says nothing.
    for value in ["", "256color", "yes", "true"] {
        assert!(
            !truecolor_requested(Some(value), Some("xterm")),
            "{value} is not a 24-bit claim"
        );
    }
    assert!(!truecolor_requested(None, Some("xterm-256color")));
    assert!(!truecolor_requested(None, None));
    assert!(!truecolor_requested(Some(""), Some("")));
}

#[test]
fn the_palette_is_truecolor_only_when_requested() {
    assert_eq!(
        palette_mode(Some("truecolor"), Some("xterm-256color")),
        Mode::Truecolor
    );
    assert_eq!(palette_mode(None, Some("xterm-256color")), Mode::Indexed256);
}

#[test]
fn the_fallback_palette_is_indexed_and_keeps_the_hierarchy() {
    let p = indexed_palette();
    let indexed = |c: Color| matches!(c, Color::Indexed(_));
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
        assert!(indexed(c), "{c:?} is not an indexed colour");
    }
    // The roles that carry meaning apart must stay apart: text well
    // above the surfaces, the marks warm against the greys.
    assert_ne!(p.text, p.muted);
    assert_ne!(p.muted, p.dim);
    assert_ne!(p.accent, p.error);
    assert_ne!(p.accent_soft, p.accent);
    assert_ne!(p.bg, p.surface);
}

/// Fifteen roles, fifteen colours: where two of them are nearest the
/// same entry (`accent`/`error`, `surface_alt`/`selection`) the one that
/// carries less meaning steps aside, so no two roles ever draw the same
/// thing on a 256-colour terminal.
#[test]
fn the_fallback_palette_keeps_every_role_a_colour_of_its_own() {
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

#[test]
fn palette_roles_are_distinct_in_truecolor() {
    let p = TRUECOLOR;
    assert_eq!(p.accent, Color::Rgb(0xD9, 0x4B, 0x4B));
    assert_eq!(p.bg, Color::Rgb(0x16, 0x19, 0x1C));
    assert_eq!(p.code, Color::Rgb(0xC7, 0xD0, 0xC0));
    // One colour per role: the view relies on them not colliding.
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
            assert_ne!(a, b, "two truecolor roles share {a:?}");
        }
    }
}
