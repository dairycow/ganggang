//! Slash-command suggestions (§15 amendment): while the composer holds
//! a bare command word — first char `/`, no whitespace yet — a filtered
//! list of everything that word could become floats above the editor:
//! the built-ins and, once `/skill:` is typed, one row per skill. Pure,
//! like [`classify_submission`](super::commands::classify_submission): the
//! same catalog the submit path matches against, so the list can never
//! advertise a command that does not run. The row-to-`UiLine` builders
//! live here too — the popup's whole look, in the popup's module.

use crate::agent::skills::Skill;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as UiLine, Span};

use super::theme::Palette;

/// Rows the popup shows at most. Ten is the whole built-in set (the
/// merged set includes `/hooks`, §22, and `/model`); skills flow in as the
/// prefix narrows.
const MAX_ROWS: usize = 10;

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
        "summarise and rebuild the context",
    ),
    ("/skills", "", "list discovered skills"),
    ("/session", "", "session path, entries, token totals"),
    ("/context", "", "show the assembled system prompt"),
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
/// built-ins, then (once `/skill:` is typed) per-skill rows. Empty when
/// the popup should not show.
pub fn suggestions(text: &str, skills: &[Skill]) -> Vec<Suggestion> {
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
    out.truncate(MAX_ROWS);
    out
}

/// One popup row as the terminal sees it: the command first, the
/// description to its right, and the selected row marked out — a scarlet
/// `▸` in its left margin, the row on `selection`, the command bold in body
/// text. Unselected rows are pale grey on the popup's raised surface, the
/// command kept bold so the eye lands on the word it is matching, not the
/// description.
///
/// Chars beyond `width` drop, piecewise across the spans — the popup never
/// wraps. Char-wise like [`wrap_line`](super::line::wrap_line): a multibyte
/// character is never split, a wide glyph may render a row slightly long and
/// the terminal clips.
pub(super) fn suggestion_row(
    palette: &Palette,
    suggestion: &Suggestion,
    selected: bool,
    width: usize,
) -> UiLine<'static> {
    let row = palette.popup_row(selected);
    let mark = if selected { palette.popup_mark() } else { row };
    let command = row.add_modifier(Modifier::BOLD);
    let description = Style {
        fg: Some(if selected { palette.text } else { palette.dim }),
        ..row
    };
    // The marker column exists only on a selected row, and it is part of
    // the row's width: the text after it can never reach past `width`.
    let marker = if selected { "▸ " } else { "  " };
    let marker = take_chars(marker, width);
    let left = width.saturating_sub(marker.chars().count());
    let label = take_chars(&suggestion.label, left);
    let left = left.saturating_sub(label.chars().count());
    let gap = if suggestion.description.is_empty() {
        String::new()
    } else {
        take_chars("  ", left)
    };
    let left = left.saturating_sub(gap.chars().count());
    let mut spans = vec![
        Span::styled(marker, mark),
        Span::styled(label, command),
        Span::styled(gap, description),
        Span::styled(take_chars(&suggestion.description, left), description),
    ];
    // The selection runs to the popup's right edge: what the text leaves
    // of the row is the selected row's own ground, so the highlight reads
    // as a band and not as a strip as long as its description.
    if selected {
        let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
        let pad = width.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), row));
        }
    }
    UiLine::from(spans)
}

/// The popup's last row: how to drive it, dim, on the popup's surface.
pub(super) fn hint_row(palette: &Palette, width: usize) -> UiLine<'static> {
    UiLine::from(Span::styled(
        take_chars(" ↑↓ select · Tab complete · Enter sends", width),
        palette.popup_hint(),
    ))
}

/// First `n` chars of `s` — the popup's no-wrapping truncation.
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
#[cfg(test)]
mod tests;
