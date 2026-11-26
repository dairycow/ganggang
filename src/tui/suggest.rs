//! Slash-command suggestions (§15 amendment): while the composer holds
//! a bare command word — first char `/`, no whitespace yet — a filtered
//! list of every built-in command the word could become floats above the
//! editor. Pure, like
//! [`classify_submission`](super::commands::classify_submission): the
//! same catalog the submit path matches against, so the list can never
//! advertise a command that does not run. The row-to-`UiLine` builders
//! live here too, and [`picker`](super::picker) draws its rows with them.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as UiLine, Span};

use super::theme::Palette;

/// One popup row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the row shows, args hint included: `/session [filter|copy|info]`.
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
    (
        "/compact",
        " [instructions]",
        "summarise and rebuild the context",
    ),
    (
        "/session",
        " [filter|copy|info]",
        "pick a saved session, copy its path, or show session info",
    ),
    ("/context", "", "show the assembled system prompt"),
    ("/hooks", "", "list loaded hooks"),
    ("/skill", " <name> [args]", "run a skill now"),
    (
        "/model",
        " [provider/model]",
        "list or switch provider and model",
    ),
    (
        "/think",
        " [effort|off|default] [budget]",
        "show or set thinking effort and budget",
    ),
    ("/reload", "", "reload AGENTS.md and skills from disk"),
    ("/quit", "", "exit"),
];

/// The popup is live while the composer holds a bare command word (a
/// leading `/` and no whitespace yet). Everything else closes it:
/// arguments to other commands are typed with the popup gone, and so is
/// everything that is not a command.
pub fn active(text: &str) -> bool {
    text.starts_with('/') && !text.chars().any(char::is_whitespace)
}

/// The built-in suggestions for the composer's current text, in catalog
/// order. The three list commands are rows while they are still being
/// typed (`/ses` suggests `/session`), and stop being one once typed in
/// full: the list is open then, and the popup never shows over a list.
/// Empty when the popup should not show.
pub fn suggestions(text: &str) -> Vec<Suggestion> {
    let out: Vec<Suggestion> = BUILTINS
        .iter()
        .filter(|(name, _, _)| name.starts_with(text))
        .filter(|(name, _, _)| {
            !(matches!(*name, "/session" | "/model" | "/skill") && *name == text)
        })
        .map(|(name, args_hint, description)| Suggestion {
            label: format!("{name}{args_hint}"),
            // Every command completes with a trailing space, ready for
            // arguments.
            completion: format!("{name} "),
            description: (*description).to_string(),
        })
        .collect();
    if active(text) { out } else { Vec::new() }
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
pub(super) fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
#[cfg(test)]
mod tests;
