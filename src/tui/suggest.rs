//! Slash-command suggestions (§15 amendment): while the composer holds
//! a bare command word — first char `/`, no whitespace yet — a filtered
//! list of everything that word could become floats above the editor:
//! the built-ins, the discovered prompt templates (`/<name>`, M7.4),
//! and, once `/skill:` is typed, one row per skill. Pure, like
//! [`classify_submission`](super::commands::classify_submission): the
//! same catalog the submit path matches against, so the list can never
//! advertise a command that does not run. The row-to-`UiLine` builders
//! live here too — the popup's whole look, in the popup's module.

use crate::agent::skills::Skill;
use crate::prompts::PromptTemplate;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};

/// Rows the popup shows at most. Eleven is the whole built-in set (the
/// merged set includes `/hooks`, §22, and `/model`); templates and skills
/// flow in as the prefix narrows.
const MAX_ROWS: usize = 11;

/// One popup row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the row shows, args hint included: `/resume [n|prefix]`.
    pub label: String,
    /// What Tab puts in the composer: the command plus a trailing space,
    /// ready for arguments.
    pub completion: String,
    /// The dim right-hand text.
    pub description: String,
}

/// The built-in half of the catalog: (command, args hint, description).
/// `/exit` stays a hidden alias — the popup advertises one way out.
pub(crate) const BUILTINS: &[(&str, &str, &str)] = &[
    ("/new", "", "start a fresh session"),
    ("/resume", " [n|prefix]", "list sessions, or open one"),
    (
        "/compact",
        " [instructions]",
        "summarize and rebuild the context",
    ),
    ("/skills", "", "list discovered skills"),
    ("/session", "", "session path, entries, token totals"),
    ("/context", "", "show the assembled system prompt"),
    ("/prompts", "", "list prompt templates"),
    ("/hooks", "", "list loaded hooks"),
    ("/skill:", "<name> [args]", "run a skill now"),
    (
        "/model",
        " [provider/model]",
        "list or switch provider and model",
    ),
    ("/quit", "", "exit"),
];

/// The popup is live only while the composer is exactly a command word:
/// a leading `/` and no whitespace yet — arguments are typed with the
/// popup gone, and so is everything that is not a command.
pub fn active(text: &str) -> bool {
    text.starts_with('/') && !text.chars().any(char::is_whitespace)
}

/// The suggestions for the composer's current text, in catalog order:
/// built-ins, then templates, then (once `/skill:` is typed) per-skill
/// rows. Empty when the popup should not show.
pub fn suggestions(text: &str, skills: &[Skill], prompts: &[PromptTemplate]) -> Vec<Suggestion> {
    if !active(text) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(prefix) = text.strip_prefix("/skill:") {
        // The colon is typed: the generic row gives way to one row per
        // matching skill, named by what follows the colon. A skill name
        // with whitespace is skipped — the submit path splits at the
        // first space, so such a row could never run (and the popup
        // itself closes on whitespace, so it could not even be typed).
        for skill in skills {
            if skill.name.starts_with(prefix) && !skill.name.chars().any(char::is_whitespace) {
                out.push(Suggestion {
                    label: format!("/skill:{}", skill.name),
                    completion: format!("/skill:{} ", skill.name),
                    description: skill.description.clone(),
                });
            }
        }
        out.truncate(MAX_ROWS);
        return out;
    }
    for (name, args_hint, description) in BUILTINS {
        if name.starts_with(text) {
            out.push(Suggestion {
                label: format!("{name}{args_hint}"),
                // Every command completes with a trailing space, ready
                // for arguments — except the generic `/skill:` row,
                // which completes to the colon: the name is still to
                // come, and a space there would close the popup.
                completion: if *name == "/skill:" {
                    (*name).to_string()
                } else {
                    format!("{name} ")
                },
                description: (*description).to_string(),
            });
        }
    }
    for template in prompts {
        let command = format!("/{}", template.name);
        if command.starts_with(text) {
            out.push(Suggestion {
                completion: format!("{command} "),
                description: template.description.clone(),
                label: command,
            });
        }
    }
    out.truncate(MAX_ROWS);
    out
}

/// One popup row as the terminal sees it: the command bold, the
/// description dim to its right, the highlighted row inverted
/// (terminal-native selection look). Chars beyond `width` drop,
/// piecewise across the spans — the popup never wraps. Char-wise like
/// `view::wrap_line`: a multibyte character is never split, a wide
/// glyph may render a row slightly long and the terminal clips.
pub(super) fn suggestion_row(
    suggestion: &Suggestion,
    selected: bool,
    width: usize,
) -> UiLine<'static> {
    let reversed = if selected {
        Modifier::REVERSED
    } else {
        Modifier::empty()
    };
    let command = Style::new().add_modifier(Modifier::BOLD | reversed);
    let description = Style::new()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::ITALIC | reversed);
    let label = take_chars(&suggestion.label, width);
    let left = width.saturating_sub(label.chars().count());
    let gap = if suggestion.description.is_empty() {
        String::new()
    } else {
        take_chars("  ", left)
    };
    let left = left.saturating_sub(gap.chars().count());
    UiLine::from(vec![
        Span::styled(label, command),
        Span::styled(gap, description),
        Span::styled(take_chars(&suggestion.description, left), description),
    ])
}

/// The popup's last row: how to drive it, dim.
pub(super) fn hint_row(width: usize) -> UiLine<'static> {
    UiLine::from(Span::styled(
        take_chars(" ↑↓ select · Tab complete · Enter sends", width),
        Style::new().fg(Color::DarkGray),
    ))
}

/// First `n` chars of `s` — the popup's no-wrapping truncation.
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn template(name: &str, description: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.into(),
            description: description.into(),
            path: PathBuf::from(format!("/tmp/prompts/{name}.md")),
            body: String::new(),
        }
    }

    fn labels(text: &str, skills: &[Skill], prompts: &[PromptTemplate]) -> Vec<String> {
        suggestions(text, skills, prompts)
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    #[test]
    fn a_bare_slash_lists_the_whole_builtin_set() {
        let got = labels("/", &[], &[]);
        assert_eq!(
            got,
            vec![
                "/new",
                "/resume [n|prefix]",
                "/compact [instructions]",
                "/skills",
                "/session",
                "/context",
                "/prompts",
                "/hooks",
                "/skill:<name> [args]",
                "/model [provider/model]",
                "/quit",
            ],
            "catalog order, /exit stays hidden"
        );
    }

    #[test]
    fn the_typed_prefix_filters_and_keeps_order() {
        assert_eq!(labels("/re", &[], &[]), vec!["/resume [n|prefix]"]);
        assert_eq!(
            labels("/c", &[], &[]),
            vec!["/compact [instructions]", "/context"]
        );
        // `/skill` is a prefix of both the list command and the forcing
        // form — both are real continuations, both show.
        assert_eq!(
            labels("/sk", &[], &[]),
            vec!["/skills", "/skill:<name> [args]"]
        );
        assert_eq!(labels("/skills", &[], &[]), vec!["/skills"]);
        assert_eq!(labels("/zzz", &[], &[]), Vec::<String>::new());
    }

    #[test]
    fn completions_carry_a_trailing_space_for_arguments() {
        let got = suggestions("/res", &[], &[]);
        assert_eq!(got[0].completion, "/resume ");
        // The generic skill row completes to the colon, not a space —
        // the name is still to come.
        let got = suggestions("/ski", &[], &[]);
        assert_eq!(got[1].completion, "/skill:");
    }

    #[test]
    fn the_typed_colon_expands_one_row_per_matching_skill() {
        let skills = [
            skill("pdf-tools", "pdf extraction"),
            skill("pdftk", "the other pdf one"),
            skill("web", "fetch pages"),
        ];
        assert_eq!(
            labels("/skill:", &skills, &[]),
            vec!["/skill:pdf-tools", "/skill:pdftk", "/skill:web"]
        );
        assert_eq!(
            labels("/skill:pdf", &skills, &[]),
            vec!["/skill:pdf-tools", "/skill:pdftk"]
        );
        assert_eq!(labels("/skill:zzz", &skills, &[]), Vec::<String>::new());
        let got = suggestions("/skill:web", &skills, &[]);
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
            labels("/skill:pdf", &skills, &[]),
            vec!["/skill:pdf-tools"],
            "the spaced imposter is skipped (it would match the prefix), the real one shows"
        );
    }

    #[test]
    fn prompt_templates_appear_after_the_builtins() {
        let prompts = [template("review", "review the diff")];
        assert_eq!(labels("/rev", &[], &prompts), vec!["/review"]);
        let got = suggestions("/review", &[], &prompts);
        assert_eq!(got[0].description, "review the diff");
        assert_eq!(got[0].completion, "/review ");
        // A template shadowing a built-in name cannot occur — the
        // loader skips built-in names (prompts.rs BUILTINS) — so the
        // popup never has to break that tie.
    }

    #[test]
    fn anything_that_is_not_a_bare_command_word_closes_the_list() {
        let prompts = [template("review", "d")];
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
            assert_eq!(suggestions(text, &[], &prompts), Vec::new(), "{text:?}");
            assert!(!active(text), "{text:?}");
        }
    }

    #[test]
    fn the_cap_keeps_the_list_at_ten_rows() {
        let prompts: Vec<_> = (0..12)
            .map(|i| template(&format!("t{i:02}"), "filler"))
            .collect();
        assert_eq!(suggestions("/", &[], &prompts).len(), MAX_ROWS);
        let skills: Vec<_> = (0..12).map(|i| skill(&format!("s{i:02}"), "f")).collect();
        assert_eq!(suggestions("/skill:", &skills, &[]).len(), MAX_ROWS);
    }

    /// What one row's spans say, joined — the truncation contract:
    /// label first, gap only when a description exists, everything
    /// within `width` chars, never wrapped.
    fn row_text(line: &UiLine) -> String {
        line.spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn rows_truncate_to_the_width_and_never_wrap() {
        let s = Suggestion {
            label: "/resume [n|prefix]".into(),
            completion: "/resume ".into(),
            description: "list sessions, or open one".into(),
        };
        // Wide enough: label, two spaces, description.
        let full = suggestion_row(&s, false, 80);
        assert_eq!(
            row_text(&full),
            "/resume [n|prefix]  list sessions, or open one"
        );
        // Narrow: the label itself is cut, no gap, no description.
        let cut = suggestion_row(&s, true, 8);
        assert_eq!(row_text(&cut), "/resume ");
        assert!(cut.spans[0].style.add_modifier.contains(Modifier::REVERSED));
        // Width 0 draws nothing and does not panic.
        assert_eq!(row_text(&suggestion_row(&s, false, 0)), "");
        assert_eq!(row_text(&hint_row(0)), "");
        // The hint truncates too.
        assert!(row_text(&hint_row(10)).starts_with(" ↑↓"));
    }

    #[test]
    fn a_row_without_a_description_has_no_gap() {
        let s = Suggestion {
            label: "/quit".into(),
            completion: "/quit ".into(),
            description: String::new(),
        };
        let line = suggestion_row(&s, false, 80);
        assert_eq!(row_text(&line), "/quit");
        assert_eq!(
            line.spans.len(),
            3,
            "span shape is uniform, empty tail spans"
        );
    }

    /// Multibyte labels truncate at char boundaries — an é or 你 is
    /// never split mid-byte.
    #[test]
    fn multibyte_labels_truncate_on_char_boundaries() {
        let s = Suggestion {
            label: "/héllo你好".into(),
            completion: "/héllo你好 ".into(),
            description: String::new(),
        };
        assert_eq!(row_text(&suggestion_row(&s, false, 3)), "/hé");
    }
}
