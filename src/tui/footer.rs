//! The one-line footer: where we are, who we talk to, what it cost, and
//! whether a run is live. Pure state → string; mod.rs styles it.

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
    }

    /// The stopping mark goes with a busy run and nowhere else: a run that
    /// ends (completed, failed, or aborted) clears it here as well.
    pub fn set_stopping(&mut self, stopping: bool) {
        self.stopping = stopping && self.busy;
    }

    /// The footer's last segment, separately: the renderer gives the
    /// stopping mark its own colour, and asks for it rather than searching
    /// a rendered string for it.
    pub fn status_text(&self) -> &'static str {
        if !self.busy {
            return "○ ready";
        }
        if self.stopping {
            "● stopping…"
        } else {
            "● working"
        }
    }

    /// Whether the footer is showing the stopping mark.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// The whole line at once: the assertion shape of the footer's tests.
    /// The renderer wants [`render_split`](Self::render_split), which keeps
    /// the status mark apart to style it.
    #[cfg(test)]
    pub fn render(&self) -> String {
        let (body, status) = self.render_split();
        format!("{body}{status}")
    }

    /// e.g. `…/projects/ganggang │ qwen3.8 │ ↑305 ↓81 │ ctx 4% │ ● working`
    ///
    /// The footer in the two pieces the renderer needs: everything up to
    /// the status mark, and the mark itself. The mark gets its own style
    /// while a run is stopping; the rest of the line is one colour.
    pub fn render_split(&self) -> (String, &'static str) {
        let context = self
            .context_pct
            .map(|pct| format!("│ ctx {pct}% "))
            .unwrap_or_default();
        let watch = self
            .watch
            .as_deref()
            .map(|watch| format!("│ {watch} "))
            .unwrap_or_default();
        let body = format!(
            "{} │ {} │ ↑{} ↓{} {}{}│ ",
            self.cwd_short, self.model, self.tokens_in, self.tokens_out, context, watch,
        );
        (body, self.status_text())
    }
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
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn footer_shows_location_model_tokens_and_state() {
        let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
        f.set_usage(305, 81);
        f.set_busy(true);
        let rendered = f.render();
        assert!(rendered.contains("Projects/ganggang"), "{rendered}");
        assert!(rendered.contains("qwen3.8"));
        assert!(rendered.contains("↑305 ↓81"));
        assert!(rendered.contains("● working"));
        f.set_busy(false);
        assert!(f.render().contains("○ ready"));
    }

    #[test]
    fn a_run_asked_to_stop_says_so_in_the_footer() {
        let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
        f.set_busy(true);
        assert!(f.render().contains("● working"));
        f.set_stopping(true);
        let rendered = f.render();
        assert!(rendered.contains("● stopping…"), "{rendered}");
        assert!(!rendered.contains("● working"), "{rendered}");
        // Stopping is a state of a live run: it does not outlive it.
        f.set_busy(false);
        let ready = f.render();
        assert!(ready.contains("○ ready"), "{ready}");
        assert!(!ready.contains("stopping"), "{ready}");
        f.set_busy(true);
        assert!(f.render().contains("● working"), "and it is off after that");
    }

    #[test]
    fn watch_status_renders_only_when_watching() {
        let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
        assert!(!f.render().contains("watch"));
        f.set_watch(Some("watch debug · watching".into()));
        assert!(f.render().contains("watch debug · watching"));
        f.set_watch(None);
        assert!(!f.render().contains("watch"));
    }

    #[test]
    fn short_paths_are_shown_whole() {
        assert_eq!(shorten(Path::new("/")), "/");
        assert_eq!(shorten(Path::new("/home")), "/home");
    }
}
