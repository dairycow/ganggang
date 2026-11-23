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
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// e.g. `…/projects/ganggang │ qwen3.8 │ ↑305 ↓81 │ ctx 4% │ ● working`
    pub fn render(&self) -> String {
        let context = self
            .context_pct
            .map(|pct| format!("│ ctx {pct}% "))
            .unwrap_or_default();
        let watch = self
            .watch
            .as_deref()
            .map(|watch| format!("│ {watch} "))
            .unwrap_or_default();
        format!(
            "{} │ {} │ ↑{} ↓{} {}{}│ {}",
            self.cwd_short,
            self.model,
            self.tokens_in,
            self.tokens_out,
            context,
            watch,
            if self.busy {
                "● working"
            } else {
                "○ ready"
            }
        )
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
