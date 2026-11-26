//! The palette: one place that decides what every colour in the TUI
//! means, themed on the Australian gang-gang cockatoo — charcoal plumage,
//! a scarlet crest, orange-red cheeks, a warm cream beak, pale grey wings
//! and a grey-green wash on the wing coverts.
//!
//! Two palettes exist and exactly one is chosen, once, at startup:
//! truecolor when the terminal claims it ([`truecolor_requested`]), and the
//! xterm-256 approximation the [`fallback`] module derives from these same
//! values otherwise. Everything else in the view asks the [`Palette`] for a
//! role (`text`, `accent`, `dim`) and never names a colour, so the theme can
//! be retuned without touching a widget.

use std::env;

use ratatui::style::{Color, Modifier, Style};

/// A colour for every role the view needs. A role, not a hue: the same
/// field carries truecolor or 256-colour values depending on the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Full-screen background: the plumage.
    pub bg: Color,
    /// Composer, footer, panels: raised ground.
    pub surface: Color,
    /// Popups, code bands, expanded tool output: raised further.
    pub surface_alt: Color,
    pub border: Color,
    /// Warm off-white body text: the beak.
    pub text: Color,
    /// Pale grey: the wings, and everything secondary.
    pub muted: Color,
    /// Dimmer grey: markers, notes, thinking.
    pub dim: Color,
    /// Scarlet crest: the mark of gg itself, and of stopping.
    pub accent: Color,
    /// Orange-red cheek: live/working things — a tool call, a spinner.
    pub accent_soft: Color,
    /// Deep red: reserved for anything louder than an error line.
    pub accent_deep: Color,
    /// Grey-green wing wash: a call that worked.
    pub ok: Color,
    /// Wattle gold: a warning, a watch state, a full context window.
    pub warn: Color,
    pub error: Color,
    /// Code text, on `surface_alt`.
    pub code: Color,
    /// Background of the selected popup row.
    pub selection: Color,
    /// True black: the outline that lifts the scrollbar off the ground.
    /// The one colour darker than `bg`.
    pub outline: Color,
}

impl Default for Palette {
    /// The designed palette: what the drawing means when the terminal can
    /// do what the design asks.
    fn default() -> Self {
        TRUECOLOR
    }
}

/// The exact colours, as designed.
pub const TRUECOLOR: Palette = Palette {
    bg: Color::Rgb(0x16, 0x19, 0x1C),
    surface: Color::Rgb(0x20, 0x24, 0x29),
    surface_alt: Color::Rgb(0x2A, 0x2F, 0x35),
    border: Color::Rgb(0x3A, 0x40, 0x47),
    text: Color::Rgb(0xE9, 0xE3, 0xD7),
    muted: Color::Rgb(0x98, 0xA0, 0xA6),
    dim: Color::Rgb(0x6B, 0x73, 0x79),
    accent: Color::Rgb(0xD9, 0x4B, 0x4B),
    accent_soft: Color::Rgb(0xE0, 0x7A, 0x4F),
    accent_deep: Color::Rgb(0x8E, 0x30, 0x30),
    ok: Color::Rgb(0xA7, 0xB3, 0x8F),
    warn: Color::Rgb(0xD9, 0xA4, 0x41),
    error: Color::Rgb(0xE0, 0x6A, 0x6A),
    code: Color::Rgb(0xC7, 0xD0, 0xC0),
    selection: Color::Rgb(0x3A, 0x2B, 0x2B),
    outline: Color::Rgb(0x00, 0x00, 0x00),
};

/// Whether the terminal was asked for 24-bit colour: `COLORTERM` says
/// `truecolor`/`24bit`, or `TERM` advertises direct colour (the
/// `*-direct` family, e.g. `xterm-256color-direct`). Pure so the rule is
/// testable without a terminal; the caller supplies the environment.
pub fn truecolor_requested(colorterm: Option<&str>, term: Option<&str>) -> bool {
    let deep = |value: &str| {
        let value = value.to_ascii_lowercase();
        value == "truecolor" || value == "24bit"
    };
    if colorterm.is_some_and(deep) {
        return true;
    }
    term.is_some_and(|term| term.to_ascii_lowercase().contains("direct"))
}

/// The palette the process draws with: truecolor when the terminal claims
/// it, the 256-colour approximation otherwise. Read once, at startup, and
/// never again — a terminal's claim does not change mid-session.
pub fn palette_from_env() -> Palette {
    let (colorterm, term) = (env::var("COLORTERM").ok(), env::var("TERM").ok());
    let mode = palette_mode(colorterm.as_deref(), term.as_deref());
    match mode {
        Mode::Truecolor => TRUECOLOR,
        Mode::Indexed256 => indexed_palette(),
    }
}

/// Which palette a pair of environment values selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Truecolor,
    Indexed256,
}

fn palette_mode(colorterm: Option<&str>, term: Option<&str>) -> Mode {
    if truecolor_requested(colorterm, term) {
        Mode::Truecolor
    } else {
        Mode::Indexed256
    }
}

// ---------------------------------------------------------------- styles --

impl Palette {
    /// The frame under everything: painted across the whole terminal
    /// before a widget draws, so the theme owns the screen rather than
    /// leaving the terminal's own background showing through.
    pub fn frame(&self) -> Style {
        Style::new().fg(self.text).bg(self.bg)
    }

    /// One transcript line by kind (see
    /// [`LineKind`](super::transcript::LineKind)): what a line IS stays in
    /// the transcript model, what it looks like stays here. A line's marks
    /// — its prefix, a tool call's arguments, a status glyph — are layered
    /// on top of this by the marks in [`line`](super::line).
    pub fn line(&self, kind: super::transcript::LineKind) -> Style {
        use super::transcript::LineKind as K;
        match kind {
            K::User => Style::new().fg(self.text),
            K::Assistant => Style::new().fg(self.text),
            K::Code => Style::new().fg(self.code).bg(self.surface_alt),
            K::Table => Style::new().fg(self.text),
            K::Tool | K::ToolError => Style::new().fg(self.accent_soft),
            K::Output => Style::new().fg(self.dim).bg(self.surface_alt),
            K::Info => Style::new().fg(self.dim),
            K::Warning => Style::new().fg(self.warn),
            K::CommandError => Style::new().fg(self.error),
            K::Aborted => Style::new().fg(self.accent).add_modifier(Modifier::BOLD),
            K::Thinking => Style::new().fg(self.dim).add_modifier(Modifier::ITALIC),
        }
    }

    /// The `> ` mark of a user message, and the abort marker: the crest,
    /// bold. Scarlet against the model's cream text, so the two never read
    /// as one speaker.
    pub fn user_mark(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }

    /// The welcome block's three rows: the name, the line under it, the
    /// keys.
    pub fn welcome(&self) -> Style {
        Style::new().fg(self.muted)
    }

    pub fn welcome_name(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }

    pub fn welcome_keys(&self) -> Style {
        Style::new().fg(self.dim)
    }

    /// The composer box: `surface` with a `border` edge, the border going
    /// warm while a run is live and scarlet while it is being stopped. Two
    /// styles, because a `Block` takes its background and its border colour
    /// separately.
    pub fn composer(&self, busy: bool, stopping: bool) -> (Style, Style) {
        let edge = if stopping {
            self.accent
        } else if busy {
            self.accent_soft
        } else {
            self.border
        };
        (
            Style::new().bg(self.surface),
            Style::new().fg(edge).bg(self.surface),
        )
    }

    /// The popup's panel: raised surface, dim hint row, selected row on
    /// `selection` with a scarlet marker.
    pub fn popup(&self) -> Style {
        Style::new().bg(self.surface_alt)
    }

    pub fn popup_row(&self, selected: bool) -> Style {
        if selected {
            Style::new()
                .fg(self.text)
                .bg(self.selection)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(self.muted).bg(self.surface_alt)
        }
    }

    pub fn popup_hint(&self) -> Style {
        Style::new().fg(self.dim).bg(self.surface_alt)
    }

    /// The selected popup row's left marker: the crest on the selection.
    pub fn popup_mark(&self) -> Style {
        Style::new()
            .fg(self.accent)
            .bg(self.selection)
            .add_modifier(Modifier::BOLD)
    }

    /// Scrollbar: a scarlet thumb on the border colour, no arrows. Each
    /// cell of the bar's one column is black on the left and the lane on
    /// the right (the glyphs are set where the bar is drawn), so the text
    /// never touches it. It lives in the transcript's own column, so both
    /// parts carry `outline`.
    pub fn scrollbar(&self) -> (Style, Style) {
        (
            Style::new().fg(self.accent).bg(self.outline),
            Style::new().fg(self.border).bg(self.outline),
        )
    }
}
mod fallback;

pub use fallback::indexed_palette;
#[cfg(test)]
mod tests;
