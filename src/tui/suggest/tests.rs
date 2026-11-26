//! Tests for [`super`]: the catalog, its filtering and cap, and how a row is drawn.

use super::*;

use crate::tui::theme::TRUECOLOR;
use std::path::PathBuf;

fn skill(name: &str, description: &str) -> Skill {
    Skill {
        name: name.into(),
        description: description.into(),
        disable_model_invocation: false,
        path: PathBuf::from(format!("/tmp/{name}/SKILL.md")),
        instructions: String::new(),
    }
}

fn labels(text: &str, skills: &[Skill]) -> Vec<String> {
    suggestions(text, skills)
        .into_iter()
        .map(|s| s.label)
        .collect()
}

#[test]
fn a_bare_slash_lists_the_whole_builtin_set() {
    let got = labels("/", &[]);
    assert_eq!(
        got,
        vec![
            "/new",
            "/compact [instructions]",
            "/session [filter|copy|info]",
            "/context",
            "/hooks",
            "/skill:<name> [args]",
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
    assert_eq!(labels("/re", &[]), vec!["/reload"]);
    assert_eq!(labels("/ses", &[]), vec!["/session [filter|copy|info]"]);
    assert_eq!(
        labels("/c", &[]),
        vec!["/compact [instructions]", "/context"]
    );
    // `/sk` only matches the forcing form: the list command is gone.
    assert_eq!(labels("/sk", &[]), vec!["/skill:<name> [args]"]);
    assert_eq!(labels("/skills", &[]), Vec::<String>::new());
    assert_eq!(labels("/zzz", &[]), Vec::<String>::new());
}

#[test]
fn completions_carry_a_trailing_space_for_arguments() {
    let got = suggestions("/ses", &[]);
    assert_eq!(got[0].completion, "/session ");
    // The generic skill row completes to the colon, not a space —
    // the name is still to come.
    let got = suggestions("/ski", &[]);
    assert_eq!(got[0].completion, "/skill:");
}

#[test]
fn the_typed_colon_expands_one_row_per_matching_skill() {
    let skills = [
        skill("pdf-tools", "pdf extraction"),
        skill("pdftk", "the other pdf one"),
        skill("web", "fetch pages"),
    ];
    assert_eq!(
        labels("/skill:", &skills),
        vec!["/skill:pdf-tools", "/skill:pdftk", "/skill:web"]
    );
    assert_eq!(
        labels("/skill:pdf", &skills),
        vec!["/skill:pdf-tools", "/skill:pdftk"]
    );
    assert_eq!(labels("/skill:zzz", &skills), Vec::<String>::new());
    let got = suggestions("/skill:web", &skills);
    assert_eq!(got[0].description, "fetch pages");
    assert_eq!(got[0].completion, "/skill:web ");
}

/// Skill names are validated leniently (§11: non-empty only), so a
/// name with whitespace CAN be discovered — but `classify_submission`
/// splits at the first space, making such a row unrunnable (and the
/// popup itself closes on whitespace, so it is untypeable too). The
/// catalog skips it rather than advertise a lie.
#[test]
fn whitespace_named_skills_are_not_advertised() {
    let skills = [
        skill("pdf tools", "spaced"),
        skill("pdf-tools", "hyphenated"),
    ];
    assert_eq!(
        labels("/skill:pdf", &skills),
        vec!["/skill:pdf-tools"],
        "the spaced imposter is skipped (it would match the prefix), the real one shows"
    );
}

/// A `disable-model-invocation` skill is flagged in its row: the popup
/// says what the system prompt does not.
#[test]
fn manual_only_skills_are_flagged_in_their_row() {
    let mut manual = skill("pdf-tools", "pdf extraction");
    manual.disable_model_invocation = true;
    let skills = [manual, skill("web", "fetch pages")];
    let got = suggestions("/skill:", &skills);
    assert_eq!(got[0].description, "pdf extraction · manual only");
    assert_eq!(got[1].description, "fetch pages");
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
        "/model x", // the filter is the list's, not the popup's
    ] {
        assert_eq!(suggestions(text, &[]), Vec::new(), "{text:?}");
        assert!(!active(text), "{text:?}");
    }
    // The built-in row, alone: the model rows live in the list.
    assert_eq!(suggestions("/model", &[]).len(), 1, "the generic row");
    assert!(active("/mo"), "a partial command word is the popup's");
    assert_eq!(
        suggestions("/model", &[])[0].label,
        "/model [provider/model]"
    );
}

#[test]
fn the_cap_keeps_the_list_at_the_builtin_set() {
    assert_eq!(
        suggestions("/", &[]).len(),
        BUILTINS.len(),
        "the whole built-in set"
    );
    let skills: Vec<_> = (0..14).map(|i| skill(&format!("s{i:02}"), "f")).collect();
    assert!(
        suggestions("/skill:", &skills).len() > 1,
        "the rows flow in"
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
