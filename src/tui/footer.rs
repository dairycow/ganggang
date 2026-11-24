//! The one-line footer: where we are, who we talk to, what it cost, and
//! whether a run is live.
//!
//! State in, [`UiLine`] out — the footer owns its own styling now, because
//! a footer made of structured spans cannot be styled as one string: the
//! `gg` mark, the context percentage's colour, and a spinner that changes
//! frame to frame all need their own span. The colour decisions that carry
//! meaning (how full the context window may get before the number turns
//! gold, then red; which spinner frame is showing) are pure functions of
//! state, so they are testable without a terminal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as UiLine, Span};

use super::theme::Palette;

/// Spinner frames, one per 100 ms tick. Braille, eight states of one
/// rotating dot: quiet enough to sit in a status line without demanding
/// the eye.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The context percentage at which the footer's number turns gold, and
/// the one past which it turns red. Under the first the number is a fact;
/// between them it is worth noticing; above it, the run is about to
/// compact.
pub const CTX_WARN: u8 = 60;
pub const CTX_ERROR: u8 = 85;

/// Footer contents, updated as events arrive.
#[derive(Debug)]
pub struct Footer {
    cwd_short: String,
    model: String,
    tokens_in: u64,
    tokens_out: u64,
    /// Context usage of the last request vs the window, once known.
    context_pct: Option<u8>,
    /// `--watch` state, absent when not watching (dev mode only).
    watch: Option<String>,
    busy: bool,
    /// Ctrl+C asked a running task to stop: still busy, but on the way out
    /// (gap §5). The footer says so instead of the plain working mark.
    stopping: bool,
    /// Spinner frame, advanced by the event loop only while a run is live.
    frame: usize,
}

impl Footer {
    pub fn new(cwd: &std::path::Path, model: &str) -> Self {
        Self {
            cwd_short: shorten(cwd),
            model: model.to_string(),
            tokens_in: 0,
            tokens_out: 0,
            context_pct: None,
            watch: None,
            busy: false,
            stopping: false,
            frame: 0,
        }
    }

    /// The watch line, e.g. `watch debug · watching` or `rebuild ready`.
    pub fn set_watch(&mut self, watch: Option<String>) {
        self.watch = watch;
    }

    pub fn set_usage(&mut self, tokens_in: u64, tokens_out: u64) {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
    }

    /// §15's context %: the last request's prompt+completion tokens
    /// against the configured window (which the footer does not know —
    /// the caller has the config).
    pub fn set_context(&mut self, context_tokens: u64, context_window: u64) {
        if context_window == 0 || context_tokens == 0 {
            self.context_pct = None;
            return;
        }
        self.context_pct = Some((context_tokens.saturating_mul(100) / context_window) as u8);
        if self.context_pct > Some(100) {
            self.context_pct = Some(100);
        }
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        // The stopping mark belongs to one run: it goes whenever the busy
        // mark moves, in either direction, and a run asked to stop is
        // marked again with [`set_stopping`](Self::set_stopping).
        self.stopping = false;
        // A run that starts begins its spinner at the top; one that ends
        // leaves no half-turn behind.
        self.frame = 0;
    }

    /// The stopping mark goes with a busy run and nowhere else: a run that
    /// ends (completed, failed, or aborted) clears it here as well.
    pub fn set_stopping(&mut self, stopping: bool) {
        self.stopping = stopping && self.busy;
    }

    /// Whether a spinner has anything to show: the loop asks before it
    /// arms its 100 ms timer, so an idle footer never wakes the frame.
    pub fn spinning(&self) -> bool {
        self.busy
    }

    /// Advance the spinner one frame. Called from the event loop's timer
    /// arm only while [`spinning`](Self::spinning).
    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % SPINNER.len();
    }

    /// The spinner frame the footer is showing.
    pub fn spinner(&self) -> char {
        SPINNER[self.frame % SPINNER.len()]
    }

    /// Whether the footer is showing the stopping mark.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// The whole line at once, unstyled — what the footer's tests assert
    /// against, and the text behind [`spans`](Self::spans). Test-only by
    /// design: the view draws spans, and nothing in the program needs the
    /// flat form.
    #[cfg(test)]
    pub fn render(&self) -> String {
        self.spans(&Palette::default())
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// e.g. `gg …/projects/ganggang qwen3.8 ↑305 ↓81 ctx 4% ● working`
    ///
    /// One row of spans, each already carrying the style its role means:
    /// the mark in the crest, the location in pale grey, the model in body
    /// text, the totals and the context number in muted (the number going
    /// gold then red as the window fills), the watch state in gold, and
    /// the status — ready, a turning spinner and `working`, or `stopping…`.
    pub fn spans(&self, palette: &Palette) -> UiLine<'static> {
        let muted = Style::new().fg(palette.muted);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(
                "gg",
                Style::new().fg(palette.accent).add_modifier(Modifier::BOLD),
            ),
            field(muted),
            Span::styled(self.cwd_short.clone(), muted),
            field(muted),
            Span::styled(self.model.clone(), Style::new().fg(palette.text)),
            field(muted),
            Span::styled(format!("↑{} ↓{}", self.tokens_in, self.tokens_out), muted),
        ];
        if let Some(pct) = self.context_pct {
            spans.push(field(muted));
            spans.push(Span::styled(
                format!("ctx {pct}%"),
                Style::new().fg(Self::ctx_colour(palette, pct)),
            ));
        }
        if let Some(watch) = &self.watch {
            spans.push(field(muted));
            spans.push(Span::styled(watch.clone(), Style::new().fg(palette.warn)));
        }
        spans.push(field(muted));
        spans.extend(self.status_spans(palette));
        UiLine::from(spans)
    }

    /// The status in the spans its state needs: one for the word, so a
    /// stopping run is scarlet and bold while the spinner stays warm.
    fn status_spans(&self, palette: &Palette) -> Vec<Span<'static>> {
        if !self.busy {
            return vec![Span::styled("○ ready", Style::new().fg(palette.muted))];
        }
        if self.stopping {
            return vec![
                Span::styled(self.spinner().to_string(), Style::new().fg(palette.accent)),
                Span::styled(
                    " stopping…",
                    Style::new().fg(palette.accent).add_modifier(Modifier::BOLD),
                ),
            ];
        }
        vec![
            Span::styled(
                self.spinner().to_string(),
                Style::new().fg(palette.accent_soft),
            ),
            Span::styled(" working", Style::new().fg(palette.accent_soft)),
        ]
    }

    /// The colour of the context percentage: a fact, then a warning, then
    /// a problem. Pure, and the rule the footer test pins down.
    pub fn ctx_colour(palette: &Palette, pct: u8) -> ratatui::style::Color {
        if pct <= CTX_WARN {
            palette.muted
        } else if pct <= CTX_ERROR {
            palette.warn
        } else {
            palette.error
        }
    }
}

/// One field boundary: ` │ ` in the pale-grey field colour, so the footer
/// reads as fields rather than one long sentence.
fn field(style: Style) -> Span<'static> {
    Span::styled(" │ ", style)
}

/// Last two components of the path — enough to tell projects apart,
/// short enough for one line. Short paths (≤ 2 components) are shown whole.
fn shorten(cwd: &std::path::Path) -> String {
    let parts: Vec<_> = cwd.components().collect();
    if parts.len() <= 2 {
        return cwd.display().to_string();
    }
    let parent = parts[parts.len() - 2].as_os_str().to_string_lossy();
    let leaf = parts[parts.len() - 1].as_os_str().to_string_lossy();
    format!("…/{parent}/{leaf}")
}
#[cfg(test)]
mod tests;
