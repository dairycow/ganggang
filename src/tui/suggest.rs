//! Slash-command suggestions (§15 amendment): while the composer holds
//! a bare command word — first char `/`, no whitespace yet — a filtered
//! list of everything that word could become floats above the editor:
//! the built-ins and, once `/skill:` is typed, one row per skill. Pure,
//! like [`classify_submission`](super::commands::classify_submission): the
//! same catalog the submit path matches against, so the list can never
//! advertise a command that does not run. The row-to-`UiLine` builders
//! live here too — the popup's whole look, in the popup's module.

use crate::agent::skills::Skill;
use crate::config::providers::CatalogRow;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as UiLine, Span};

use super::theme::Palette;

/// Rows the popup shows at most. Eleven is the whole built-in set (the
/// merged set includes `/hooks`, §22, `/model`, `/think`, and `/reload`);
/// skills flow in as the prefix narrows.
const MAX_ROWS: usize = 11;

/// One model row the `/model` popup lists from: the catalog's row, minus
/// what the popup does not show (`api`), so the TUI never names the
/// transport directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    pub context_window: u32,
    pub status: Option<String>,
    pub needs_key: bool,
    pub has_key: bool,
}

impl From<&CatalogRow> for ModelChoice {
    fn from(row: &CatalogRow) -> Self {
        Self {
            provider: row.provider.clone(),
            model: row.model.clone(),
            context_window: row.context_window,
            status: row.status.clone(),
            needs_key: row.needs_key,
            has_key: row.has_key,
        }
    }
}

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
    (
        "/resume",
        " [n|prefix]",
        "pick a saved session from any directory",
    ),
    (
        "/compact",
        " [instructions]",
        "summarise and rebuild the context",
    ),
    ("/session", "", "session path, entries, token totals"),
    ("/context", "", "show the assembled system prompt"),
    ("/hooks", "", "list loaded hooks"),
    ("/skill:", "<name> [args]", "run a skill now"),
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
/// leading `/` and no whitespace yet), or `/model ` plus a filter word,
/// which completes the argument in place. Everything else closes it:
/// arguments to other commands are typed with the popup gone, and so is
/// everything that is not a command.
pub fn active(text: &str) -> bool {
    if text.starts_with('/') && !text.chars().any(char::is_whitespace) {
        return true;
    }
    let Some(rest) = text.strip_prefix("/model ") else {
        return false;
    };
    !rest.chars().any(char::is_whitespace)
}

/// The suggestions for the composer's current text, in catalog order:
/// built-ins, then (once `/skill:` is typed) per-skill rows, or (while
/// `/model` or `/model <filter>` is typed) the model rows. Empty when the
/// popup should not show.
pub fn suggestions(
    text: &str,
    skills: &[Skill],
    models: &[ModelChoice],
    active_label: &str,
) -> Vec<Suggestion> {
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
                let mut description = skill.description.clone();
                if skill.disable_model_invocation {
                    description.push_str(" · manual only");
                }
                out.push(Suggestion {
                    label: format!("/skill:{}", skill.name),
                    completion: format!("/skill:{} ", skill.name),
                    description,
                });
            }
        }
        out.truncate(MAX_ROWS);
        return out;
    }
    if text == "/model" {
        // The bare command lists the built-in row (no argument yet) and
        // then every model row, so Tab on the first row just adds the
        // space the filter is typed into.
        out.push(Suggestion {
            label: "/model [provider/model]".to_string(),
            completion: "/model ".to_string(),
            description: "list or switch provider and model".to_string(),
        });
        out.extend(models.iter().map(|m| model_row(m, active_label)));
        out.truncate(MAX_ROWS);
        return out;
    }
    if let Some(filter) = text.strip_prefix("/model ")
        && !filter.chars().any(char::is_whitespace)
    {
        // A filter word: only the matching model rows, in catalog order.
        let filter = filter.to_lowercase();
        for m in models {
            let label = format!("{}/{}", m.provider, m.model);
            if label.to_lowercase().contains(&filter) {
                out.push(model_row(m, active_label));
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

/// One model row: the `provider/model` label, a `/model provider/model`
/// completion (no trailing space — the argument IS the completion), and
/// the context window, discovery status, key state and active mark in
/// the description.
fn model_row(m: &ModelChoice, active_label: &str) -> Suggestion {
    let label = format!("{}/{}", m.provider, m.model);
    let mut description = format!("ctx {}k", m.context_window / 1000);
    if let Some(status) = &m.status {
        description.push_str(&format!(" · {status}"));
    }
    if m.needs_key && !m.has_key {
        description.push_str(" · no key");
    }
    if label == active_label {
        description.push_str(" · active");
    }
    Suggestion {
        label,
        completion: format!("/model {}/{}", m.provider, m.model),
        description,
    }
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
