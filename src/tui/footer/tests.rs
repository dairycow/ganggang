//! Tests for [`super`]: the fields it owns, the spans they become, the spinner, and the context percentage's traffic light.

use super::*;

use ratatui::style::Color;
use std::path::Path;

fn footer() -> Footer {
    let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
    f.set_usage(305, 81, 0);
    f
}

fn text(f: &Footer) -> String {
    f.spans(&Palette::default())
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("")
}

fn colour(f: &Footer, needle: &str) -> Option<Color> {
    f.spans(&super::super::theme::TRUECOLOR)
        .spans
        .iter()
        .find(|s| s.content.contains(needle))
        .and_then(|s| s.style.fg)
}

#[test]
fn footer_shows_location_model_tokens_and_state() {
    let mut f = footer();
    f.set_busy(true);
    f.frame = 0;
    let rendered = text(&f);
    assert!(rendered.contains("…/Projects/ganggang"), "{rendered}");
    assert!(rendered.contains("qwen3.8"));
    assert!(rendered.contains("↑305 ↓81"));
    assert!(rendered.contains("working"), "{rendered}");
    f.set_busy(false);
    assert!(text(&f).contains("○ ready"));
}

/// The footer is not one grey string any more: the mark, the model,
/// the totals and the status each own a span, and the spans say so.
#[test]
fn the_footer_is_spans_not_a_string() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_watch(Some("watch debug · watching".into()));
    f.set_context(80_000, 100_000);
    let line = f.spans(&T);
    assert_eq!(
        line.spans[0].content, "gg",
        "the mark leads, and it is the crest"
    );
    assert_eq!(line.spans[0].style.fg, Some(T.accent));
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(colour(&f, "qwen3.8"), Some(T.text));
    assert_eq!(colour(&f, "…/Projects/ganggang"), Some(T.muted));
    assert_eq!(colour(&f, "↑305 ↓81"), Some(T.muted));
    assert_eq!(colour(&f, "watch debug"), Some(T.warn));
    assert_eq!(colour(&f, "○ ready"), Some(T.muted));
}

/// The context number is a traffic light, at the documented bounds.
#[test]
fn the_context_number_darkens_as_the_window_fills() {
    use crate::tui::theme::TRUECOLOR as T;
    assert_eq!(Footer::ctx_colour(&T, 0), T.muted);
    assert_eq!(
        Footer::ctx_colour(&T, CTX_WARN),
        T.muted,
        "60% is still a fact"
    );
    assert_eq!(Footer::ctx_colour(&T, CTX_WARN + 1), T.warn);
    assert_eq!(
        Footer::ctx_colour(&T, CTX_ERROR),
        T.warn,
        "85% is a warning"
    );
    assert_eq!(Footer::ctx_colour(&T, CTX_ERROR + 1), T.error);
    assert_eq!(Footer::ctx_colour(&T, 100), T.error);

    let mut f = footer();
    f.set_context(61_000, 100_000);
    assert_eq!(colour(&f, "ctx 61%"), Some(T.warn));
    f.set_context(4_000, 100_000);
    assert_eq!(colour(&f, "ctx 4%"), Some(T.muted));
    // No usage yet: no number at all, not a zero.
    let fresh = Footer::new(Path::new("/tmp/x"), "m");
    assert!(!text(&fresh).contains("ctx"));
}

#[test]
fn the_mark_is_gg_at_rest_and_a_pair_of_wings_while_a_run_works() {
    let mut f = footer();
    assert_eq!(f.mark(), "gg", "at rest the mark is the name, not a blank");
    f.set_busy(true);

    // The pair fills the same two cells the name did, so nothing behind
    // the mark moves while the turn turns.
    let mut pairs = Vec::new();
    for _ in 0..WINGS.len() {
        let mark = f.mark();
        assert_eq!(mark.chars().count(), 2, "the name's two cells: {mark:?}");
        assert_ne!(mark, "gg");
        pairs.push(mark);
        f.tick();
    }
    // A whole turn comes round to the start.
    assert_eq!(f.mark(), pairs[0], "wrapped, not reset by itself");
    assert_eq!(pairs.len(), WINGS.len());
    assert_eq!(
        pairs,
        vec!["◤◤", "◤◥", "◥◤", "◣◢", "◣◢", "◤◤"]
            .into_iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>(),
    );
    // The mark goes back to the name when the run ends.
    f.set_busy(false);
    assert_eq!(f.mark(), "gg");
    f.set_busy(true);
    assert_eq!(f.mark(), "◤◤", "a new run starts on the crest");
}

#[test]
fn the_wings_beat_in_the_order_they_draw_in() {
    // Left wing first in every frame, and the right one trailing it, so
    // the pair reads as two marks beating rather than one glyph swapping.
    let wings = ["◤", "◥", "◣", "◢"];
    for (left, right) in WINGS {
        assert!(wings.contains(&left), "left wing: {left}");
        assert!(wings.contains(&right), "right wing: {right}");
    }
    // The pair rises together on the first frame, beats over the top,
    // holds the descent for a frame, and the turn ends where it began.
    let pairs: Vec<_> = WINGS[0..2].iter().map(|(l, r)| (*l, *r)).collect();
    assert_eq!(
        pairs,
        [("◤", "◤"), ("◤", "◥")],
        "the wings rise, then beat over the top"
    );
    let pairs: Vec<_> = WINGS[3..5].iter().map(|(l, r)| (*l, *r)).collect();
    assert_eq!(
        pairs,
        [("◣", "◢"), ("◣", "◢")],
        "the descent is held a frame"
    );
    assert_eq!(WINGS[5], WINGS[0], "the turn ends where it began");
    // No glyph is used that this font would draw as nothing.
    for (left, right) in WINGS {
        for glyph in [left, right] {
            assert!(
                !glyph.is_empty() && !glyph.chars().all(char::is_control),
                "glyph {glyph:?}"
            );
        }
    }
}

/// On brand: the wings flash scarlet on the crest frame and are the
/// cheek's orange-red the rest of the turn, so the crest is the one frame
/// that reads as `gg`'s own mark rather than as a working state.
#[test]
fn the_wings_flash_the_crest_once_a_turn() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_busy(true);
    for i in 0..WINGS.len() {
        let line = f.spans(&T);
        assert_eq!(
            line.spans[0].content,
            f.mark(),
            "the mark is the frame the footer is on"
        );
        let want = if i == 0 { T.accent } else { T.accent_soft };
        assert_eq!(
            line.spans[0].style.fg,
            Some(want),
            "frame {i} of {WINGS:?} should be {}",
            if i == 0 { "the crest" } else { "the cheek" }
        );
        f.tick();
    }
    // One flash per turn, not one per frame.
    assert_eq!(f.frame % WINGS.len(), 0);
    f.set_busy(false);
}

#[test]
fn a_run_asked_to_stop_says_so_in_the_footer() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_busy(true);
    assert!(text(&f).contains("working"));
    f.set_stopping(true);
    let rendered = text(&f);
    assert!(rendered.contains("stopping…"), "{rendered}");
    assert!(!rendered.contains("working"), "{rendered}");
    assert_eq!(colour(&f, "stopping…"), Some(T.accent), "and in scarlet");
    // Stopping is a state of a live run: it does not outlive it.
    f.set_busy(false);
    let ready = text(&f);
    assert!(ready.contains("○ ready"), "{ready}");
    assert!(!ready.contains("stopping"), "{ready}");
    f.set_busy(true);
    assert!(text(&f).contains("working"), "and it is off after that");
}

#[test]
fn the_model_s_thinking_level_sits_beside_its_name() {
    let mut f = footer();
    assert!(!text(&f).contains("think"), "unset: no field at all");

    f.set_thinking(Some("high"), None);
    let shown = text(&f);
    assert!(shown.contains("qwen3.8 │ high │ ↑305"), "{shown}");

    // `none` switches thinking off rather than setting a level.
    f.set_thinking(Some("none"), None);
    assert!(text(&f).contains("│ think off │"), "{}", text(&f));
    f.set_thinking(Some("none"), Some(4096));
    assert!(text(&f).contains("think off"), "{}", text(&f));

    // A budget alone is a cap, not a level.
    f.set_thinking(None, Some(4096));
    assert!(text(&f).contains("│ 4096t │"), "{}", text(&f));

    // An effort wins over the budget: the level is what the user chose.
    f.set_thinking(Some(" medium "), Some(4096));
    assert!(text(&f).contains("│ medium │"), "{}", text(&f));

    // Empty effort and zero budget mean the provider's defaults.
    f.set_thinking(Some(""), Some(0));
    assert!(!text(&f).contains("think"), "{}", text(&f));
}

#[test]
fn watch_status_renders_only_when_watching() {
    let mut f = footer();
    assert!(!text(&f).contains("watch"));
    f.set_watch(Some("watch debug · watching".into()));
    assert!(text(&f).contains("watch debug · watching"));
    f.set_watch(None);
    assert!(!text(&f).contains("watch"));
}

#[test]
fn the_plain_render_matches_the_spans_for_the_assertion_shape() {
    let mut f = footer();
    f.set_busy(true);
    let rendered = f.render();
    assert!(rendered.contains("↑305 ↓81"), "{rendered}");
    assert!(rendered.contains("working"), "{rendered}");
    f.set_busy(false);
    assert!(f.render().contains("○ ready"));
}

#[test]
fn short_paths_are_shown_whole() {
    assert_eq!(shorten(Path::new("/")), "/");
    assert_eq!(shorten(Path::new("/home")), "/home");
}

/// The throughput figures are fields like the rest: there when the backend
/// reports them, absent when it does not, and gone with a new session.
#[test]
fn telemetry_renders_tps_and_thinking_pct_when_set() {
    let mut f = footer();
    let plain = text(&f);
    assert!(
        !plain.contains("t/s") && !plain.contains("think"),
        "{plain}"
    );

    f.set_decode_tps(Some(62.4));
    f.set_thinking_pct(Some(45));
    let shown = text(&f);
    assert!(shown.contains("62.4 t/s"), "{shown}");
    assert!(shown.contains("think 45%"), "{shown}");
    // Both are facts, not alarms: muted like the token counts.
    assert_eq!(colour(&f, "t/s"), Some(Color::Rgb(0x98, 0xA0, 0xA6)));
    assert_eq!(colour(&f, "think"), Some(Color::Rgb(0x98, 0xA0, 0xA6)));

    f.clear_telemetry();
    let cleared = text(&f);
    assert!(!cleared.contains("t/s"), "{cleared}");
    assert!(!cleared.contains("think"), "{cleared}");
    assert!(!cleared.contains("ctx"), "{cleared}");
}

#[test]
fn set_context_clamps_above_100_without_u8_wrapping() {
    let mut f = footer();
    f.set_context(400_000, 100_000);
    assert!(text(&f).contains("ctx 100%"), "{}", text(&f));
}

/// The cache hit rate is a field like the rest: it appears once the
/// provider reports cache reads, muted like the token counts, clamped at
/// 100, and gone when there is nothing cached.
#[test]
fn the_cache_hit_rate_sits_beside_the_context_pct() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    assert!(!text(&f).contains("cache"), "unset: no field at all");

    f.set_usage(1000, 50, 0);
    f.set_cache(870, 1000);
    let shown = text(&f);
    assert!(shown.contains("↑1000 ↓50 │ cache 87%"), "{shown}");
    assert_eq!(colour(&f, "cache"), Some(T.muted), "a fact, not an alarm");

    // The rate is clamped, never above 100.
    f.set_cache(100, 100);
    assert!(text(&f).contains("cache 100%"), "{}", text(&f));

    // Zero cache reads: the field is simply not there.
    f.set_cache(0, 1000);
    assert!(!text(&f).contains("cache"), "{}", text(&f));

    // Reads with no prompt to divide by: no field either.
    f.set_cache(5, 0);
    assert!(!text(&f).contains("cache"), "{}", text(&f));
}
