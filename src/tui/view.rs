//! TUI view state and drawing: the transcript model (in [`transcript`])
//! plus the editor, footer, and bottom-anchored scrolling.

use super::footer::Footer;
use super::input::Editor;
use super::suggest::{hint_row, suggestion_row};
use super::transcript::{LineKind, RenderedLine, Transcript};
use crate::events::AgentEvent;
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Everything the draw closure needs; owned by the event loop.
pub struct State {
    pub editor: Editor,
    pub footer: Footer,
    pub busy: bool,
    transcript: Transcript,
    /// Transcript scroll, measured from the BOTTOM: 0 pins to the newest
    /// row (auto-follow); paging up grows it. New content arriving keeps
    /// the view anchored wherever the user left it.
    offset_from_bottom: u16,
    /// Wrapped-row count from the last render — the clamp for scrolling.
    last_row_count: u16,
    /// What `/session` reports; `None` path = not persisted.
    pub session_path: Option<std::path::PathBuf>,
    pub sessions_disabled: bool,
    /// Totals from the last run (§13: summed across the session).
    pub entries: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The last `/resume` listing, for `/resume <n>`.
    pub pending_resume: Vec<crate::agent::session::io::SessionFile>,
    /// The last `/model` listing, for `/model <n>`.
    pub pending_models: Vec<(String, String)>,
    /// Configured context window — the denominator of the footer's %.
    context_window: u64,
    last_context_tokens: u64,
    /// The discovery sets the suggestion popup lists from (§15
    /// amendment): skills for `/skill:<name>` rows, prompt templates
    /// for `/<name>` rows. Shared read-only with the command handler.
    skills: std::sync::Arc<Vec<crate::agent::skills::Skill>>,
    prompts: std::sync::Arc<Vec<crate::prompts::PromptTemplate>>,
    /// Highlighted row of the suggestion popup. Resets to the top
    /// whenever the composer text changes (render syncs it); ↑/↓ move
    /// it, Tab completes from it.
    suggest_selected: usize,
    /// The composer text the selection was last synced against.
    suggest_filter: String,
    /// Rows the popup last actually DREW (render publishes it, like
    /// `last_row_count`): a short terminal shows fewer rows than the
    /// list holds, and the selection must never point past what is on
    /// screen — Tab would complete a row the user cannot see. 0 = not
    /// drawn yet (popup keys clamp to the full list, as tests do).
    suggest_view_rows: usize,
}

impl State {
    pub fn new(
        cwd: &std::path::Path,
        model: &str,
        context_window: u32,
        skills: std::sync::Arc<Vec<crate::agent::skills::Skill>>,
        prompts: std::sync::Arc<Vec<crate::prompts::PromptTemplate>>,
    ) -> Self {
        Self {
            editor: Editor::default(),
            footer: Footer::new(cwd, model),
            busy: false,
            transcript: Transcript::default(),
            offset_from_bottom: 0,
            last_row_count: 0,
            session_path: None,
            sessions_disabled: false,
            entries: 0,
            tokens_in: 0,
            tokens_out: 0,
            pending_resume: Vec::new(),
            pending_models: Vec::new(),
            context_window: context_window as u64,
            last_context_tokens: 0,
            skills,
            prompts,
            suggest_selected: 0,
            suggest_filter: String::new(),
            suggest_view_rows: 0,
        }
    }

    /// A run reported its last request's context usage.
    pub fn set_context_tokens(&mut self, context_tokens: u64) {
        self.last_context_tokens = context_tokens;
        self.footer.set_context(context_tokens, self.context_window);
    }

    pub fn set_model(&mut self, label: &str, context_window: u32) {
        self.footer.set_model(label);
        self.context_window = context_window as u64;
        self.footer
            .set_context(self.last_context_tokens, self.context_window);
    }

    pub fn push_user(&mut self, text: &str) {
        self.transcript.push_user(text);
    }

    pub fn push_info(&mut self, text: &str) {
        self.transcript.push_info(text);
    }

    pub fn push_warning(&mut self, message: &str) {
        self.transcript.apply(&AgentEvent::Warned {
            message: message.to_string(),
        });
    }

    pub fn push_shell(
        &mut self,
        command: &str,
        output: &str,
        exit_code: Option<i32>,
        to_context: bool,
    ) {
        self.transcript
            .push_shell(command, output, exit_code, to_context);
    }

    /// The last assistant text, for Ctrl+X.
    pub fn last_answer(&self) -> Option<String> {
        self.transcript.last_answer().map(String::from)
    }

    /// Every path that starts work: busy flag + footer indicator.
    pub fn begin_run(&mut self) {
        self.busy = true;
        self.footer.set_busy(true);
    }

    pub fn record_totals(&mut self, tokens_in: u64, tokens_out: u64, entries: usize) {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self.entries = entries;
    }

    /// Seed the view from persisted entries (resume): transcript blocks
    /// from the disk-known conversation, footer totals from its usage.
    /// Without this a resumed session renders blank and reports 0 tokens
    /// until the first new run.
    pub fn seed_from_entries(&mut self, entries: &[crate::agent::session::Entry]) {
        self.transcript.extend_from_entries(entries);
        let (tokens_in, tokens_out) = super::agent_task::usage_totals(entries);
        self.footer.set_usage(tokens_in, tokens_out);
        self.record_totals(tokens_in, tokens_out, entries.len());
    }

    /// `/new` / `/resume`: the view starts describing the new session.
    /// The transcript keeps prior content — it is what the human saw.
    pub fn new_session(
        &mut self,
        session_path: Option<std::path::PathBuf>,
        sessions_disabled: bool,
    ) {
        self.session_path = session_path;
        self.sessions_disabled = sessions_disabled;
        self.entries = 0;
        self.tokens_in = 0;
        self.tokens_out = 0;
        let where_ = self
            .session_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not persisted".into());
        self.transcript
            .push_info(&format!("— new session — {where_}"));
    }

    /// Run-level failures render as warnings in the transcript — the
    /// same dim shape `Warned` events use.
    pub fn push_failure(&mut self, error: &str) {
        self.transcript.apply(&AgentEvent::Warned {
            message: format!("run failed: {error}"),
        });
    }

    /// Command failures (a bad /resume index, a missing skill) are local
    /// mistakes, not run failures — their own shape, no alarm prefix.
    pub fn push_command_error(&mut self, message: &str) {
        self.transcript.push_command_error(message);
    }

    pub fn apply(&mut self, event: &AgentEvent) {
        self.transcript.apply(event);
    }

    /// Ctrl+O: expand the most recent collapsed call; when all are
    /// expanded, collapse them all (the transcript's documented rule).
    pub fn toggle_expand(&mut self) {
        self.transcript.toggle_expand();
    }

    /// Ctrl+T: show/hide the model's thinking (stored either way).
    pub fn toggle_thinking(&mut self) {
        self.transcript.toggle_thinking();
    }

    /// The live suggestion list for what the composer holds.
    pub fn suggestions(&self) -> Vec<super::suggest::Suggestion> {
        super::suggest::suggestions(self.editor.text(), &self.skills, &self.prompts)
    }

    /// Whether the popup should draw: a bare command word that matches
    /// at least one row.
    pub fn suggest_open(&self) -> bool {
        super::suggest::active(self.editor.text()) && !self.suggestions().is_empty()
    }

    /// ↑: the highlight climbs, stopping at the top.
    pub fn suggest_prev(&mut self) {
        self.sync_suggest();
        self.suggest_selected = self.suggest_selected.saturating_sub(1);
    }

    /// ↓: the highlight descends, stopping at the last VISIBLE row —
    /// a short terminal draws fewer rows than the list holds, and an
    /// off-screen highlight is a trap (Tab completes what nobody can
    /// see). The popup does not scroll; narrowing the prefix does.
    pub fn suggest_next(&mut self) {
        self.sync_suggest();
        let last = self.last_selectable_row();
        self.suggest_selected = (self.suggest_selected + 1).min(last);
    }

    /// Tab: the highlighted row's completion replaces the composer.
    pub fn complete_suggestion(&mut self) {
        self.sync_suggest();
        let list = self.suggestions();
        let last = self.last_selectable_row();
        if let Some(suggestion) = list.get(self.suggest_selected.min(last)) {
            self.editor.set_text(&suggestion.completion);
        }
    }

    /// The highest row index the popup currently lets the selection
    /// reach: the last drawn row when render has published one, else
    /// the last row of the whole list.
    fn last_selectable_row(&self) -> usize {
        let len = self.suggestions().len();
        let rows = if self.suggest_view_rows == 0 {
            len
        } else {
            self.suggest_view_rows.min(len)
        };
        rows.saturating_sub(1)
    }

    /// Reset the highlight whenever the composer text changed — the
    /// list is a pure function of the text, so a changed text is a new
    /// list and the top row is the honest default again. render() calls
    /// this every frame, and so does every popup key handler: the
    /// selection is never allowed to point into a stale list.
    fn sync_suggest(&mut self) {
        if self.editor.text() != self.suggest_filter {
            self.suggest_filter = self.editor.text().to_string();
            self.suggest_selected = 0;
        }
    }

    pub fn run_finished(&mut self) {
        self.busy = false;
        self.footer.set_busy(false);
    }

    pub fn scroll_up(&mut self, by: u16) {
        // Rows, not logical lines: wrapping only adds rows, so the line
        // count is the pre-render lower bound and the render pass
        // publishes the exact figure each frame.
        let lower_bound = self.transcript.render().len() as u16;
        let max = self.last_row_count.max(lower_bound).saturating_sub(1);
        self.offset_from_bottom = (self.offset_from_bottom + by).min(max);
    }

    pub fn scroll_down(&mut self, by: u16) {
        self.offset_from_bottom = self.offset_from_bottom.saturating_sub(by);
    }

    // Test seams: the transcript and scroll offset are view-internal; the
    // tests sit inside the module tree and reach in through these.
    #[cfg(test)]
    pub(crate) fn line_kinds_for_test(&self) -> Vec<LineKind> {
        self.transcript
            .render()
            .into_iter()
            .map(|l| l.kind)
            .collect()
    }

    /// Park the popup's drawn-row count, as render would publish it —
    /// short-terminal clamping without a terminal.
    #[cfg(test)]
    pub(crate) fn set_suggest_view_rows_for_test(&mut self, rows: usize) {
        self.suggest_view_rows = rows;
    }

    #[cfg(test)]
    pub(crate) fn line_texts_for_test(&self) -> Vec<String> {
        self.transcript
            .render()
            .into_iter()
            .map(|l| l.text)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn offset_for_test(&self) -> u16 {
        self.offset_from_bottom
    }
}

/// Draw one frame: transcript (fills, wrapped), editor box (5 rows),
/// footer (1). Takes `&mut State` to publish the wrapped-row count the
/// scroll clamp needs.
pub fn render(frame: &mut Frame, state: &mut State) {
    use ratatui::layout::{Constraint, Layout};

    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Pre-wrapped rows, bottom-anchored: long lines wrap instead of
    // clipping, and scrolling counts the rows the terminal shows.
    let lines = state.transcript.render();
    let width = chunks[0].width as usize;
    let rows: Vec<RenderedLine> = lines.iter().flat_map(|l| wrap_line(l, width)).collect();
    let total_rows = rows.len() as u16;
    state.last_row_count = total_rows;
    let top = total_rows.saturating_sub(state.offset_from_bottom + chunks[0].height);
    let ui_lines: Vec<UiLine> = rows.iter().skip(top as usize).map(styled_line).collect();
    frame.render_widget(Paragraph::new(ui_lines), chunks[0]);

    let editor_lines: Vec<UiLine> = state.editor.lines().into_iter().map(UiLine::from).collect();
    frame.render_widget(
        Paragraph::new(editor_lines).block(Block::default().borders(Borders::TOP)),
        chunks[1],
    );

    // Borders::TOP only: text starts one row down and at the very left
    // edge — no left inset to add, and the top border row is not text.
    let (row, col) = state.editor.cursor_row_col();
    let inner_height = chunks[1].height.saturating_sub(1);
    let row = row.min(inner_height.saturating_sub(1) as usize);
    let col = col.min(chunks[1].width.saturating_sub(1) as usize);
    frame.set_cursor_position((chunks[1].x + col as u16, chunks[1].y + 1 + row as u16));

    // Slash-command suggestions: a floating list over the transcript,
    // directly above the editor, while the composer holds a bare
    // command word. Clear wipes the rows underneath so the popup reads
    // on any background; the height caps to the room above the editor,
    // so a tiny terminal just shows fewer rows — and publishes how many
    // it drew, so the selection can never walk past the screen.
    state.sync_suggest();
    let list = state.suggestions();
    state.suggest_view_rows = 0;
    if !list.is_empty() {
        let height = (list.len() as u16 + 1).min(chunks[1].y);
        if height >= 2 {
            let area = ratatui::layout::Rect {
                x: chunks[1].x,
                y: chunks[1].y - height,
                width: chunks[1].width,
                height,
            };
            frame.render_widget(ratatui::widgets::Clear, area);
            let shown = (height as usize - 1).min(list.len());
            state.suggest_view_rows = shown;
            let selected = state.suggest_selected.min(shown - 1);
            let mut popup = Vec::with_capacity(height as usize);
            for (i, row) in list.iter().take(shown).enumerate() {
                popup.push(suggestion_row(row, i == selected, area.width as usize));
            }
            popup.push(hint_row(area.width as usize));
            frame.render_widget(Paragraph::new(popup), area);
        }
    }

    frame.render_widget(
        Paragraph::new(UiLine::from(state.footer.render())).style(Style::new().fg(Color::Gray)),
        chunks[2],
    );
}

/// Split one rendered line into rows of at most `width` chars. Char-wise,
/// so a multibyte character is never split; wide glyphs (CJK) may render a
/// row slightly long — the terminal clips the excess, nothing panics, and
/// the row count stays exact for scrolling. Bold char-ranges are clipped
/// to their row, so a span split across rows bolds both pieces.
pub(crate) fn wrap_line(line: &RenderedLine, width: usize) -> Vec<RenderedLine> {
    if width == 0 {
        return vec![line.clone()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    // Char offset of `current`'s first char within the full line —
    // where the row under construction starts.
    let mut row_start = 0usize;
    for ch in line.text.chars() {
        if count == width {
            rows.push(finish_row(std::mem::take(&mut current), line, row_start));
            row_start += width;
            count = 0;
        }
        current.push(ch);
        count += 1;
    }
    rows.push(finish_row(current, line, row_start));
    rows
}

/// Materialize one wrapped row with its share of the line's bold ranges
/// (shifted to row-local char offsets; empty ranges drop).
fn finish_row(text: String, line: &RenderedLine, row_start: usize) -> RenderedLine {
    let row_end = row_start + text.chars().count();
    let mut bold = Vec::new();
    for (start, end) in &line.bold {
        let lo = (*start).clamp(row_start, row_end);
        let hi = (*end).clamp(row_start, row_end);
        if lo < hi {
            bold.push((lo - row_start, hi - row_start));
        }
    }
    RenderedLine::new(text, line.kind).with_bold(bold)
}

fn styled_line(line: &RenderedLine) -> UiLine<'static> {
    let style = match line.kind {
        LineKind::User => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Assistant => Style::new(),
        LineKind::Code => Style::new().fg(Color::Cyan),
        LineKind::Tool => Style::new().fg(Color::Blue),
        LineKind::ToolError => Style::new().fg(Color::Red),
        LineKind::Dim => Style::new()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    };
    if line.bold.is_empty() {
        return UiLine::from(vec![Span::styled(line.text.clone(), style)]);
    }
    // Bold ranges are sorted and disjoint (the markdown pass builds them
    // left to right); walk the char stream once, splitting on them.
    let chars: Vec<char> = line.text.chars().collect();
    let mut spans = Vec::new();
    let mut pos = 0usize;
    for (start, end) in &line.bold {
        if *start > pos {
            spans.push(Span::styled(
                chars[pos..*start].iter().collect::<String>(),
                style,
            ));
        }
        spans.push(Span::styled(
            chars[*start..*end].iter().collect::<String>(),
            style.add_modifier(Modifier::BOLD),
        ));
        pos = *end;
    }
    if pos < chars.len() {
        spans.push(Span::styled(chars[pos..].iter().collect::<String>(), style));
    }
    UiLine::from(spans)
}
