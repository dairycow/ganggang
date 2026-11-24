//! Tests for [`super`]: the fields it owns, the spans they become, the spinner, and the context percentage's traffic light.

use super::*;

use ratatui::style::Color;
use std::path::Path;

fn footer() -> Footer {
    let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
    f.set_usage(305, 81);
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
fn the_spinner_turns_only_while_a_run_is_live() {
    let mut f = footer();
    assert!(!f.spinning(), "idle must not be on a timer");
    assert_eq!(f.spinner(), SPINNER[0]);
    f.set_busy(true);
    assert!(f.spinning());
    for _ in 0..4 {
        f.tick();
    }
    assert_eq!(f.spinner(), SPINNER[4]);
    assert!(text(&f).contains("working"));
    // A full cycle returns to the top; a run that starts does too.
    for _ in 0..SPINNER.len() {
        f.tick();
    }
    assert_eq!(f.spinner(), SPINNER[4], "wrapped, not reset by itself");
    f.set_busy(false);
    assert_eq!(f.spinner(), SPINNER[0], "a new run starts at the top");
    f.set_busy(true);
    assert_eq!(f.spinner(), SPINNER[0]);
    assert_eq!(SPINNER.len(), 10, "the documented frame count");
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
