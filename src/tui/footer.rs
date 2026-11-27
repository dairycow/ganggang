//! The one-line footer: where we are, who we talk to, what it cost, how
//! fast the last turn ran, and whether a run is live.
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

use super::theme::{BEAT_FRAMES, Palette};

/// The mark's glyph frames, one per [`crate::tui::tick::PERIOD`] tick:
/// each pair is the frame's two cells, one per element, and the cells
/// concatenate to the name's width, so nothing behind the mark moves
/// while the turn turns. Every frame spells the name; the beat is
/// colour only, the theme's [`crest_beat`](Palette::crest_beat) pulse.
/// The glyph cycle runs its three frames within the pulse's
/// forty-eight (`BEAT_FRAMES`), which is what the spinner counts.
///
/// Not a different glyph: the name is the mark, at rest and at work.
/// Print mode takes these frames too, and its stderr has no colour, so
/// there the beat is gone and the character count after `streaming…`
/// is what ticks.
pub const MARK_FRAMES: [(&str, &str); 3] = [("g", "g"), ("g", "g"), ("g", "g")];
/// The frame where the mark first wears the crest's scarlet on the way
/// round: the gang-gang's crest, raised, and the frame a new run
/// starts on.
pub const CREST_FRAME: usize = 0;

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
    /// The model's configured thinking level (`high`) or, when only a
    /// budget is set, its token cap (`4096t`).
    thinking: Option<String>,
    tokens_in: u64,
    tokens_out: u64,
    /// The session's cache-read tokens, for `/session info`.
    cache_read_tokens: u64,
    /// Last turn's cache hit rate: its cache-read tokens over its prompt
    /// tokens, once a turn reports reads. The `cache NN%` field.
    cache_pct: Option<u8>,
    /// Context usage of the last request vs the window, once known.
    context_pct: Option<u8>,
    /// Last turn's decode throughput (tokens/s), once the backend reports
    /// it.
    decode_tps: Option<f64>,
    /// Last turn's thinking share (%), the same.
    thinking_pct: Option<u8>,
    /// `--watch` state, absent when not watching (dev mode only).
    watch: Option<String>,
    busy: bool,
    /// Ctrl+C asked a running task to stop: still busy, but on the way out
    /// (gap §5). The footer's mark turns scarlet and the row adds its one
    /// status field, `stopping…`.
    stopping: bool,
    /// Spinner frame, advanced by the event loop only while a run is live.
    frame: usize,
}

impl Footer {
    pub fn new(cwd: &std::path::Path, model: &str) -> Self {
        Self {
            cwd_short: shorten(cwd),
            model: model.to_string(),
            thinking: None,
            tokens_in: 0,
            tokens_out: 0,
            cache_read_tokens: 0,
            cache_pct: None,
            context_pct: None,
            decode_tps: None,
            thinking_pct: None,
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

    pub fn set_usage(&mut self, tokens_in: u64, tokens_out: u64, cache_read_tokens: u64) {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self.cache_read_tokens = cache_read_tokens;
    }

    /// The session's cache-read tokens, for `/session`'s report.
    pub fn cache_read_tokens(&self) -> u64 {
        self.cache_read_tokens
    }

    /// Last turn's cache hit rate, `cache NN%`: cache-read tokens over
    /// prompt tokens. No reads, no prompt, or a provider that leaves the
    /// figure out: the field is simply not there.
    pub fn set_cache(&mut self, cache_read_tokens: u64, prompt_tokens: u64) {
        self.cache_pct = (cache_read_tokens > 0 && prompt_tokens > 0)
            .then(|| (cache_read_tokens.saturating_mul(100) / prompt_tokens).min(100) as u8);
    }

    /// §15's context %: the last request's prompt+completion tokens
    /// against the configured window (which the footer does not know —
    /// the caller has the config).
    pub fn set_context(&mut self, context_tokens: u64, context_window: u64) {
        if context_window == 0 || context_tokens == 0 {
            self.context_pct = None;
            return;
        }
        // Clamped before it is ever a `u8`: four times the window would
        // otherwise wrap to a reassuringly small number.
        let pct = (context_tokens.saturating_mul(100) / context_window).min(100) as u8;
        self.context_pct = Some(pct);
    }

    pub fn set_decode_tps(&mut self, tps: Option<f64>) {
        self.decode_tps = tps;
    }

    pub fn set_thinking_pct(&mut self, pct: Option<u8>) {
        self.thinking_pct = pct;
    }

    /// A new session starts with no history to measure: the context
    /// figure, the cache hit rate and both throughput figures go with it.
    pub fn clear_telemetry(&mut self) {
        self.context_pct = None;
        self.cache_pct = None;
        self.decode_tps = None;
        self.thinking_pct = None;
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        // The stopping mark belongs to one run: it goes whenever the busy
        // mark moves, in either direction, and a run asked to stop is
        // marked again with [`set_stopping`](Self::set_stopping).
        self.stopping = false;
        // A run that starts begins its spinner on the crest; one that
        // ends leaves no half-turn behind.
        self.frame = CREST_FRAME;
    }

    /// The stopping mark goes with a busy run and nowhere else: a run that
    /// ends (completed, failed, or aborted) clears it here as well.
    pub fn set_stopping(&mut self, stopping: bool) {
        self.stopping = stopping && self.busy;
    }

    /// Whether a spinner has anything to show: the loop asks before it
    /// arms its timer, so an idle footer never wakes the frame.
    pub fn spinning(&self) -> bool {
        self.busy
    }

    /// Advance the spinner one frame. Called from the event loop's timer
    /// arm only while [`spinning`](Self::spinning). The frame counts the
    /// colour pulse ([`BEAT_FRAMES`]), and the glyph cycle picks its
    /// three frames out of it by remainder.
    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % BEAT_FRAMES;
    }

    /// The colour the mark takes while a run is live: scarlet throughout
    /// a stop and, while the run works, the crest beat — the pulse in
    /// [`crest_beat`](Palette::crest_beat), which blends from the crest's
    /// scarlet out to the cheek's orange-red, up to a bright peak half
    /// way toward the cream, back down the way it came, on to the deep
    /// crest red, and home, one shade per tick.
    pub fn mark_colour(&self, palette: &Palette) -> ratatui::style::Color {
        if self.stopping {
            return palette.accent;
        }
        palette.crest_beat(self.frame)
    }

    /// Whether the footer is showing the stopping mark.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// The directory gg is working in. It changes when `/session` opens a
    /// session that started in another one.
    pub fn set_directory(&mut self, cwd: &std::path::Path) {
        self.cwd_short = shorten(cwd);
    }

    /// The thinking level shown beside the model, as the bare word: the
    /// effort when configured (`medium`, `high`, `xhigh`), or a budget
    /// alone as a token cap (`4096t`). Effort `none` switches thinking off
    /// rather than setting a level, so it says so (`think off`). Neither
    /// set: the model takes provider defaults and the field is absent.
    pub fn set_thinking(&mut self, effort: Option<&str>, budget_tokens: Option<u32>) {
        let effort = effort.map(str::trim).filter(|e| !e.is_empty());
        self.thinking = match (effort, budget_tokens.filter(|t| *t > 0)) {
            (Some("none"), _) => Some("think off".to_string()),
            (Some(effort), _) => Some(effort.to_string()),
            (None, Some(tokens)) => Some(format!("{tokens}t")),
            (None, None) => None,
        };
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

    /// The two cells at the start of the footer: the name, at rest and
    /// on every frame of a run, so the row reads
    /// `gg …/projects/ganggang qwen3.8 medium ↑305 ↓81` the same at rest
    /// and at work. The beat is colour only, in
    /// [`mark_colour`](Self::mark_colour)'s reds.
    ///
    /// The same width every frame, so nothing behind the mark moves
    /// while the turn turns: the fields after the name keep their cells.
    pub fn mark(&self) -> String {
        if self.spinning() {
            Self::mark_cells(self.frame)
        } else {
            Self::MARK_NAME.to_string()
        }
    }

    /// The two cells of the frame the run is on.
    fn mark_cells(frame: usize) -> String {
        let (left, right) = MARK_FRAMES[frame % MARK_FRAMES.len()];
        format!("{left}{right}")
    }

    /// The name the mark spells at rest.
    pub const MARK_NAME: &str = "gg";

    /// One row of spans, each already carrying the style its role means:
    /// the mark in the crest's reds, the location in pale grey, the model
    /// in body
    /// text, the thinking level in muted, the totals in muted, the context
    /// number in muted (going gold then red as the window fills), the
    /// throughput figures in muted, the watch state in gold, and, only
    /// while a run is stopping, the word `stopping…` in a field of its
    /// own. The mark at the left is the row's only spinner, and at rest
    /// and at work it is all the row says about the run: no status word,
    /// and no separator after the last field.
    pub fn spans(&self, palette: &Palette) -> UiLine<'static> {
        let muted = Style::new().fg(palette.muted);
        // The mark is its own two cells, the same two the name takes up
        // at rest, so nothing behind it moves while a turn turns.
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(
                self.mark(),
                Style::new()
                    .fg(if self.spinning() {
                        self.mark_colour(palette)
                    } else {
                        palette.accent
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            field(muted),
            Span::styled(self.cwd_short.clone(), muted),
            field(muted),
            Span::styled(self.model.clone(), Style::new().fg(palette.text)),
        ];
        if let Some(thinking) = &self.thinking {
            spans.push(field(muted));
            spans.push(Span::styled(thinking.clone(), muted));
        }
        spans.extend([
            field(muted),
            Span::styled(format!("↑{} ↓{}", self.tokens_in, self.tokens_out), muted),
        ]);
        if let Some(pct) = self.context_pct {
            spans.push(field(muted));
            spans.push(Span::styled(
                format!("ctx {pct}%"),
                Style::new().fg(Self::ctx_colour(palette, pct)),
            ));
        }
        // The last turn's KV-cache hit rate, once a turn reports cache
        // reads. Absent, the field is simply not there.
        if let Some(pct) = self.cache_pct {
            spans.push(field(muted));
            spans.push(Span::styled(format!("cache {pct}%"), muted));
        }
        // Throughput, when the backend reports it: Strata does, most
        // cloud providers do not. Absent, the field is simply not there.
        if let Some(tps) = self.decode_tps {
            spans.push(field(muted));
            spans.push(Span::styled(format!("{tps:.1} t/s"), muted));
        }
        if let Some(pct) = self.thinking_pct {
            spans.push(field(muted));
            spans.push(Span::styled(format!("think {pct}%"), muted));
        }
        if let Some(watch) = &self.watch {
            spans.push(field(muted));
            spans.push(Span::styled(watch.clone(), Style::new().fg(palette.warn)));
        }
        if let Some(status) = self.status_span(palette) {
            spans.push(field(muted));
            spans.push(status);
        }
        UiLine::from(spans)
    }

    /// The footer's one status word, and only while there is one:
    /// `stopping…` scarlet and bold, because a stop says something the
    /// mark cannot. At rest and while a run works the mark at the left of
    /// the row says it ([`mark`](Self::mark)) and the row ends at its last
    /// field; through a stop the mark is scarlet on every frame
    /// ([`mark_colour`](Self::mark_colour)) and keeps beating.
    fn status_span(&self, palette: &Palette) -> Option<Span<'static>> {
        self.stopping().then(|| {
            Span::styled(
                "stopping…",
                Style::new().fg(palette.accent).add_modifier(Modifier::BOLD),
            )
        })
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
