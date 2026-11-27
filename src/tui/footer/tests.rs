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
    assert!(
        rendered.starts_with("gg │ "),
        "the crest frame spells the name, the row as at rest: {rendered}"
    );
    assert!(rendered.ends_with("↑305 ↓81"), "no status word: {rendered}");
    f.set_busy(false);
    assert!(text(&f).starts_with("gg │ "), "{}", text(&f));
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
        "the mark leads, and it is the name at rest"
    );
    assert_eq!(line.spans[0].style.fg, Some(T.accent));
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(colour(&f, "qwen3.8"), Some(T.text));
    assert_eq!(colour(&f, "…/Projects/ganggang"), Some(T.muted));
    assert_eq!(colour(&f, "↑305 ↓81"), Some(T.muted));
    assert_eq!(colour(&f, "watch debug"), Some(T.warn));
    // At rest the row ends at its last field: no status word, and no
    // separator after the last one either.
    assert_eq!(
        line.spans.last().map(|s| s.content.as_ref()),
        Some("watch debug · watching")
    );
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
fn the_mark_is_gg_at_rest_and_while_a_run_works() {
    let mut f = footer();
    assert_eq!(f.mark(), "gg", "at rest the mark is the name");
    f.set_busy(true);

    // The mark fills the same two cells the name did, so nothing behind
    // the mark moves while the turn turns, and no frame is blank.
    let mut frames = Vec::new();
    for _ in 0..MARK_FRAMES.len() {
        let mark = f.mark();
        assert_eq!(mark, "gg", "every frame spells the name: {mark:?}");
        frames.push(mark);
        f.tick();
    }
    // A whole turn comes round to the start.
    assert_eq!(f.mark(), frames[0], "wrapped, not reset by itself");
    assert_eq!(frames.len(), MARK_FRAMES.len());
    assert_eq!(
        frames,
        vec!["gg", "gg", "gg"],
        "every frame is the name, colour only: {frames:?}"
    );
    // The mark is the name when the run ends.
    f.set_busy(false);
    assert_eq!(f.mark(), "gg");
    f.set_busy(true);
    assert_eq!(f.mark(), "gg", "a new run starts on the crest");
}

/// The row leads with the mark, in the name's two cells at the left,
/// and while a run works it has no status word: the row ends at its
/// last field and reads the same in every frame, since the beat is
/// colour only. No dot and no ring is drawn anywhere in the row. A
/// stop adds the one status field, `stopping…`, and the mark is
/// scarlet on every frame of it.
#[test]
fn the_row_leads_with_the_mark_and_has_no_status_word_in_every_frame() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_busy(true);
    for _ in 0..MARK_FRAMES.len() {
        let row = text(&f);
        assert!(
            row.starts_with("gg │ "),
            "every frame spells the name: {row:?}"
        );
        assert!(!row.contains('●'), "no dot anywhere in the row: {row:?}");
        assert!(!row.contains('○'), "the ring is gone: {row:?}");
        assert!(row.ends_with("↑305 ↓81"), "no status word: {row:?}");
        f.tick();
    }
    // A stop keeps the beat, adds the one status field, and the mark is
    // scarlet throughout it.
    f.set_stopping(true);
    for _ in 0..MARK_FRAMES.len() {
        let row = text(&f);
        assert!(
            row.starts_with("gg │ "),
            "every frame spells the name: {row:?}"
        );
        assert!(!row.contains('●'), "no dot anywhere in the row: {row:?}");
        assert!(!row.contains('○'), "the ring is gone: {row:?}");
        assert!(
            row.ends_with(" │ stopping…"),
            "the word alone, in a field of its own: {row:?}"
        );
        assert_eq!(
            f.spans(&T).spans[0].style.fg,
            Some(T.accent),
            "scarlet on every frame of a stop: {row:?}"
        );
        f.tick();
    }
}

/// The three rows, spelled out. At rest the name leads and the row ends
/// at its last field. At work the row reads the same in every frame:
/// the beat is colour only, no status word, and the fields after the
/// mark keep their cells. Stopping adds one field.
#[test]
fn the_row_at_rest_at_work_and_stopping() {
    let mut f = footer();
    f.set_thinking(Some("medium"), None);
    let rest = "gg │ …/Projects/ganggang │ qwen3.8 │ medium │ ↑305 ↓81";
    assert_eq!(text(&f), rest);

    f.set_busy(true);
    for _ in 0..MARK_FRAMES.len() {
        assert_eq!(
            text(&f),
            rest,
            "the working row is the rest row, colour only"
        );
        f.tick();
    }

    f.set_stopping(true);
    assert_eq!(text(&f), format!("{rest} │ stopping…"));
    // Stopping belongs to a live run: asked for at rest, nothing shows.
    f.set_busy(false);
    f.set_stopping(true);
    assert_eq!(text(&f), rest);
}

#[test]
fn the_frames_spell_the_name_in_the_order_they_draw_in() {
    // Every frame is the name's two cells, and the turn ends where it
    // began. Each element of a pair is one of the frame's two cells.
    assert_eq!(MARK_FRAMES[0], ("g", "g"), "frame 0: the name");
    assert_eq!(MARK_FRAMES[1], ("g", "g"), "frame 1: the name, no blank");
    assert_eq!(
        MARK_FRAMES[2], MARK_FRAMES[0],
        "the turn ends where it began"
    );
    // One cell per element, and the frames spelled out: gg, gg, gg.
    let mut spelled = Vec::new();
    for (left, right) in MARK_FRAMES {
        assert_eq!(left.chars().count(), 1, "one cell: {left:?}");
        assert_eq!(right.chars().count(), 1, "one cell: {right:?}");
        spelled.push(format!("{left}{right}"));
    }
    assert_eq!(spelled, vec!["gg", "gg", "gg"], "the turn, spelled out");
}

/// On brand: the working mark is colour only, and the colours are the
/// crest's three reds in turn, scarlet, orange-red cheek, deep crest
/// red, wrapping round; a stop holds the scarlet on every frame.
#[test]
fn the_mark_beats_through_the_crest_s_three_reds() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_busy(true);
    let cycle = [T.accent, T.accent_soft, T.accent_deep];
    for _ in 0..MARK_FRAMES.len() * 2 {
        let line = f.spans(&T);
        assert_eq!(
            line.spans[0].content,
            f.mark(),
            "the mark is the frame the footer is on"
        );
        assert_eq!(
            line.spans[0].style.fg,
            Some(cycle[f.frame % MARK_FRAMES.len()]),
            "frame {} of the cycle {cycle:?}",
            f.frame % MARK_FRAMES.len()
        );
        f.tick();
    }
    // Two turns, so the wrap is proved: the sixth frame is the first's.
    f.set_stopping(true);
    for _ in 0..MARK_FRAMES.len() {
        let line = f.spans(&T);
        assert_eq!(
            line.spans[0].style.fg,
            Some(T.accent),
            "scarlet on every frame of a stop"
        );
        f.tick();
    }
    f.set_busy(false);
}

#[test]
fn a_run_asked_to_stop_says_so_in_the_footer() {
    use crate::tui::theme::TRUECOLOR as T;
    let mut f = footer();
    f.set_busy(true);
    assert!(text(&f).ends_with("↑305 ↓81"), "working: no status word");
    f.set_stopping(true);
    let rendered = text(&f);
    assert!(rendered.ends_with("│ stopping…"), "{rendered}");
    assert_eq!(colour(&f, "stopping…"), Some(T.accent), "and in scarlet");
    // Stopping is a state of a live run: it does not outlive it.
    f.set_busy(false);
    let ready = text(&f);
    assert!(ready.starts_with("gg │ "), "{ready}");
    assert!(!ready.contains("stopping"), "{ready}");
    f.set_busy(true);
    assert!(!text(&f).contains("stopping"), "and it is off after that");
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
    // Every frame spells the name, so the row reads as it does at rest.
    assert!(rendered.starts_with("gg │ "), "{rendered}");
    f.tick();
    assert!(f.render().starts_with("gg │ "), "{}", f.render());
    f.tick();
    assert!(f.render().starts_with("gg │ "), "{}", f.render());
    f.set_busy(false);
    assert!(f.render().starts_with("gg │ "), "{}", f.render());
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
