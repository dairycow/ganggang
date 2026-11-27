//! Tests for [`super`]: the catalogue and its filtering, and how a row is
//! drawn.

use super::*;

use crate::tui::theme::TRUECOLOR;

fn labels(text: &str) -> Vec<String> {
    suggestions(text).into_iter().map(|s| s.label).collect()
}

#[test]
fn a_bare_slash_lists_the_whole_builtin_set() {
    let got = labels("/");
    assert_eq!(
        got,
        vec![
            "/new",
            "/compact [instructions]",
            "/session [filter|copy|info]",
            "/context",
            "/jobs",
            "/hooks",
            "/skill <name> [args]",
            "/model [provider/model]",
            "/think [effort|off|default] [budget]",
            "/reload",
            "/quit",
        ],
        "catalog order, /exit stays hidden"
    );
}

#[test]
fn the_typed_prefix_filters_and_keeps_order() {
    assert_eq!(labels("/re"), vec!["/reload"]);
    assert_eq!(labels("/ses"), vec!["/session [filter|copy|info]"]);
    assert_eq!(labels("/c"), vec!["/compact [instructions]", "/context"]);
    assert_eq!(labels("/jo"), vec!["/jobs"]);
    assert_eq!(labels("/sk"), vec!["/skill <name> [args]"]);
    assert_eq!(labels("/skills"), Vec::<String>::new());
    assert_eq!(labels("/zzz"), Vec::<String>::new());
}

#[test]
fn completions_carry_a_trailing_space_for_arguments() {
    assert_eq!(suggestions("/ses")[0].completion, "/session ");
    // The skill row too: what follows the space is the skill's name.
    assert_eq!(suggestions("/ski")[0].completion, "/skill ");
}

/// The three list commands are rows while they are still being typed, and
/// stop being one once typed in full: the list is open then, and the
/// popup never shows over it.
#[test]
fn a_list_command_typed_in_full_is_no_longer_a_popup_row() {
    for text in ["/session", "/model", "/skill"] {
        assert!(active(text), "{text:?} is still a bare command word");
        assert!(
            BUILTINS.iter().any(|(name, ..)| *name == text),
            "{text:?} is in the catalogue"
        );
        assert_eq!(
            suggestions(text),
            Vec::new(),
            "{text:?}: the list is open instead"
        );
    }
    assert_eq!(labels("/sess"), vec!["/session [filter|copy|info]"]);
    assert_eq!(labels("/mode"), vec!["/model [provider/model]"]);
    assert_eq!(labels("/skil"), vec!["/skill <name> [args]"]);
}

#[test]
fn anything_that_is_not_a_bare_command_word_closes_the_list() {
    for text in [
        "",
        "hello",
        "/session 2", // arguments: the command word is done
        "/new\nmore", // multi-line composer text
        "  /new",     // submission trims, the popup does not
        "!",
        "!!",
        "@file.txt",
        "/model x",    // the filter is the list's, not the popup's
        "/skill name", // and so is a skill's name
    ] {
        assert_eq!(suggestions(text), Vec::new(), "{text:?}");
        assert!(!active(text), "{text:?}");
    }
    // A partial command word is still the popup's.
    assert!(active("/mo"));
    assert_eq!(suggestions("/mo")[0].label, "/model [provider/model]");
}

#[test]
fn the_whole_catalogue_is_the_popup() {
    assert_eq!(
        suggestions("/").len(),
        BUILTINS.len(),
        "the whole built-in set, with no cap"
    );
}
/// What one row's spans say, joined — the truncation contract: the
/// marker column, the label, a gap only when a description exists,
/// everything within `width` chars, never wrapped.
fn row_text(line: &UiLine) -> String {
    line.spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<Vec<_>>()
        .join("")
}

fn suggestion(label: &str, description: &str) -> Suggestion {
    Suggestion {
        label: label.into(),
        completion: format!("{label} "),
        description: description.into(),
    }
}

/// Every row carries the marker column, selected or not: the labels
/// line up, and the marker is the only thing a highlight adds.
#[test]
fn the_marker_column_is_reserved_on_every_row() {
    let s = suggestion("/session [filter|copy|info]", "pick a saved session");
    let unselected = row_text(&suggestion_row(&TRUECOLOR, &s, false, 80));
    let selected = row_text(&suggestion_row(&TRUECOLOR, &s, true, 80));
    assert_eq!(
        unselected,
        "  /session [filter|copy|info]  pick a saved session"
    );
    assert_eq!(
        selected.trim_end(),
        "▸ /session [filter|copy|info]  pick a saved session",
        "the marker is the only thing the text of a highlight adds"
    );
}

/// The highlight is the whole row, edge to edge: a description that stops
/// early must not leave the rest of the selected row unselected.
#[test]
fn the_selection_runs_to_the_popup_edge() {
    let s = suggestion("/new", "start");
    let on = suggestion_row(&TRUECOLOR, &s, true, 40);
    let text = row_text(&on);
    assert_eq!(text.chars().count(), 40, "padded to the popup's width");
    assert!(text.starts_with("▸ /new  start"));
    let pad = on.spans.last().unwrap();
    assert_eq!(
        pad.style.bg,
        Some(TRUECOLOR.selection),
        "the pad is selected ground"
    );
    assert_eq!(pad.style.fg, Some(TRUECOLOR.text));

    // A row that fills the width has nothing to pad, and an unselected
    // row never pads: its ground is the popup's own.
    assert_eq!(
        suggestion_row(&TRUECOLOR, &s, true, 13).spans.len(),
        4,
        "▸ /new  start is 13 chars"
    );
    let off = suggestion_row(&TRUECOLOR, &s, false, 40);
    assert_eq!(off.spans.len(), 4);
    assert_eq!(row_text(&off), "  /new  start");
}

#[test]
fn rows_truncate_to_the_width_and_never_wrap() {
    let s = suggestion("/session [filter|copy|info]", "pick a saved session");
    // Narrow: the marker fits, the label is cut, no description.
    let cut = suggestion_row(&TRUECOLOR, &s, true, 8);
    assert_eq!(row_text(&cut), "▸ /sessi");
    assert_eq!(row_text(&cut).chars().count(), 8);
    // Width 0 draws nothing and does not panic.
    assert_eq!(row_text(&suggestion_row(&TRUECOLOR, &s, false, 0)), "");
    assert_eq!(row_text(&hint_row(&TRUECOLOR, 0)), "");
    // The hint truncates too.
    assert!(row_text(&hint_row(&TRUECOLOR, 10)).starts_with(" ↑↓"));
}

/// The selected row is the crest marker on `selection`, the command in
/// body text bold, the description in body text; an unselected row is
/// pale grey with a dim description.
#[test]
fn the_selected_row_is_marked_not_inverted() {
    let s = suggestion("/session", "list sessions");
    let on = suggestion_row(&TRUECOLOR, &s, true, 60);
    let off = suggestion_row(&TRUECOLOR, &s, false, 60);
    assert_eq!(on.spans[0].content, "▸ ");
    assert_eq!(on.spans[0].style.fg, Some(TRUECOLOR.accent));
    assert_eq!(on.spans[0].style.bg, Some(TRUECOLOR.selection));
    assert_eq!(on.spans[1].style.fg, Some(TRUECOLOR.text));
    assert!(on.spans[1].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(on.spans[3].style.fg, Some(TRUECOLOR.text));
    assert_eq!(off.spans[0].content, "  ");
    assert_eq!(off.spans[1].style.fg, Some(TRUECOLOR.muted));
    assert_eq!(off.spans[1].style.bg, Some(TRUECOLOR.surface_alt));
    assert_eq!(off.spans[3].style.fg, Some(TRUECOLOR.dim));
    assert!(!off.spans[1].style.add_modifier.contains(Modifier::REVERSED));
    assert_eq!(
        hint_row(&TRUECOLOR, 40).spans[0].style.fg,
        Some(TRUECOLOR.dim)
    );
}

#[test]
fn a_row_without_a_description_has_no_gap() {
    let s = suggestion("/quit", "");
    let line = suggestion_row(&TRUECOLOR, &s, false, 80);
    assert_eq!(row_text(&line), "  /quit");
    assert_eq!(
        line.spans.len(),
        4,
        "span shape is uniform, empty tail spans"
    );
}

/// Multibyte labels truncate at char boundaries — an é or 你 is
/// never split mid-byte.
#[test]
fn multibyte_labels_truncate_on_char_boundaries() {
    let s = suggestion("/héllo你好", "");
    assert_eq!(row_text(&suggestion_row(&TRUECOLOR, &s, false, 5)), "  /hé");
}
