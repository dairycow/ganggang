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
    suggestions(text, skills, &[], "")
        .into_iter()
        .map(|s| s.label)
        .collect()
}

fn model(provider: &str, model: &str, context_window: u32) -> ModelChoice {
    ModelChoice {
        provider: provider.into(),
        model: model.into(),
        context_window,
        status: None,
        needs_key: false,
        has_key: false,
    }
}

fn model_labels(text: &str, models: &[ModelChoice]) -> Vec<String> {
    suggestions(text, &[], models, "")
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
            "/resume [n|prefix]",
            "/compact [instructions]",
            "/session",
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
    assert_eq!(labels("/re", &[]), vec!["/resume [n|prefix]", "/reload"]);
    assert_eq!(labels("/res", &[]), vec!["/resume [n|prefix]"]);
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
    let got = suggestions("/res", &[], &[], "");
    assert_eq!(got[0].completion, "/resume ");
    // The generic skill row completes to the colon, not a space —
    // the name is still to come.
    let got = suggestions("/ski", &[], &[], "");
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
    let got = suggestions("/skill:web", &skills, &[], "");
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
    let got = suggestions("/skill:", &skills, &[], "");
    assert_eq!(got[0].description, "pdf extraction · manual only");
    assert_eq!(got[1].description, "fetch pages");
}

#[test]
fn anything_that_is_not_a_bare_command_word_closes_the_list() {
    for text in [
        "",
        "hello",
        "/resume 2",  // arguments: the command word is done
        "/new\nmore", // multi-line composer text
        "  /new",     // submission trims, the popup does not
        "!",
        "!!",
        "@file.txt",
    ] {
        assert_eq!(suggestions(text, &[], &[], ""), Vec::new(), "{text:?}");
        assert!(!active(text), "{text:?}");
    }
}

#[test]
fn the_cap_keeps_the_list_at_max_rows() {
    assert_eq!(
        suggestions("/", &[], &[], "").len(),
        MAX_ROWS,
        "the whole built-in set"
    );
    let skills: Vec<_> = (0..14).map(|i| skill(&format!("s{i:02}"), "f")).collect();
    assert_eq!(suggestions("/skill:", &skills, &[], "").len(), MAX_ROWS);
}

#[test]
fn a_bare_model_lists_the_builtin_then_every_model() {
    let models = [
        model("anthropic", "claude-sonnet", 200_000),
        model("openai", "gpt-5", 400_000),
    ];
    let got = suggestions("/model", &[], &models, "anthropic/claude-sonnet");
    assert_eq!(got.len(), 3, "the built-in row, then the model rows");
    assert_eq!(got[0].label, "/model [provider/model]");
    assert_eq!(got[0].completion, "/model ");
    assert_eq!(got[1].label, "anthropic/claude-sonnet");
    assert_eq!(got[2].label, "openai/gpt-5");
}

#[test]
fn a_model_filter_is_a_case_insensitive_substring() {
    let models = [
        model("anthropic", "claude-sonnet", 200_000),
        model("openai", "gpt-5", 400_000),
    ];
    assert_eq!(
        model_labels("/model CLAUDE", &models),
        vec!["anthropic/claude-sonnet"],
        "upper-case filter, lower-case label"
    );
    assert_eq!(model_labels("/model gpt", &models), vec!["openai/gpt-5"]);
    assert_eq!(
        model_labels("/model /", &models),
        vec!["anthropic/claude-sonnet", "openai/gpt-5"],
        "the slash matches both"
    );
    assert_eq!(model_labels("/model zzz", &models), Vec::<String>::new());
    // A second word is not a filter: the popup closes.
    assert_eq!(
        suggestions("/model a b", &[], &models, ""),
        Vec::new(),
        "two words is an argument, not a filter"
    );
    assert!(!active("/model a b"));
}

#[test]
fn a_model_completion_has_no_trailing_space() {
    let models = [model("anthropic", "claude-sonnet", 200_000)];
    let got = suggestions("/model", &[], &models, "");
    assert_eq!(got[1].completion, "/model anthropic/claude-sonnet");
    assert!(
        !got[1].completion.ends_with(' '),
        "the argument IS the completion"
    );
    // The built-in row keeps its space: the filter is still to come.
    assert_eq!(got[0].completion, "/model ");
}

#[test]
fn model_rows_carry_the_context_status_key_and_active_marks() {
    let mut m = model("anthropic", "claude-sonnet", 200_000);
    m.status = Some("ok".into());
    m.needs_key = true;
    m.has_key = false;
    let models = [m, model("openai", "gpt-5", 400_000)];
    let got = suggestions("/model", &[], &models, "openai/gpt-5");
    assert_eq!(got[1].description, "ctx 200k · ok · no key");
    assert_eq!(got[2].description, "ctx 400k · active");
}

#[test]
fn the_model_list_caps_at_max_rows() {
    let models: Vec<_> = (0..14)
        .map(|i| model("p", &format!("m{i:02}"), 8_192))
        .collect();
    assert_eq!(suggestions("/model", &[], &models, "").len(), MAX_ROWS);
    assert_eq!(
        model_labels("/model m", &models).len(),
        MAX_ROWS,
        "the filtered list caps too"
    );
}

#[test]
fn a_partial_model_prefix_lists_only_the_builtin_row() {
    let models = [model("anthropic", "claude-sonnet", 200_000)];
    let got = suggestions("/mod", &[], &models, "");
    assert_eq!(got.len(), 1, "the model rows wait for the space");
    assert_eq!(got[0].label, "/model [provider/model]");
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
    let s = suggestion("/resume [n|prefix]", "list sessions, or open one");
    let unselected = row_text(&suggestion_row(&TRUECOLOR, &s, false, 80));
    let selected = row_text(&suggestion_row(&TRUECOLOR, &s, true, 80));
    assert_eq!(
        unselected,
        "  /resume [n|prefix]  list sessions, or open one"
    );
    assert_eq!(
        selected.trim_end(),
        "▸ /resume [n|prefix]  list sessions, or open one",
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
    let s = suggestion("/resume [n|prefix]", "list sessions, or open one");
    // Narrow: the marker fits, the label is cut, no description.
    let cut = suggestion_row(&TRUECOLOR, &s, true, 8);
    assert_eq!(row_text(&cut), "▸ /resum");
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
    let s = suggestion("/resume", "list sessions");
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
