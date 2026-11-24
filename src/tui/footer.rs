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
    /// Last turn's decode throughput (tok/s), once known.
    decode_tps: Option<f64>,
    /// Last turn's thinking share (%), once known.
    thinking_pct: Option<u8>,
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
            decode_tps: None,
            thinking_pct: None,
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
        let pct = (context_tokens.saturating_mul(100) / context_window).min(100) as u8;
        self.context_pct = Some(pct);
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    pub fn set_decode_tps(&mut self, tps: Option<f64>) {
        self.decode_tps = tps;
    }

    pub fn set_thinking_pct(&mut self, pct: Option<u8>) {
        self.thinking_pct = pct;
    }

    pub fn clear_telemetry(&mut self) {
        self.context_pct = None;
        self.decode_tps = None;
        self.thinking_pct = None;
    }

    /// e.g. `…/projects/ganggang │ qwen3.8 │ ↑305 ↓81 │ ctx 4% │ 62.4 t/s │ ● working`
    pub fn render(&self) -> String {
        let context = self
            .context_pct
            .map(|pct| format!("│ ctx {pct}% "))
            .unwrap_or_default();
        let tps = self
            .decode_tps
            .map(|t| format!("│ {t:.1} t/s "))
            .unwrap_or_default();
        let think = self
            .thinking_pct
            .map(|p| format!("│ think {p}% "))
            .unwrap_or_default();
        let watch = self
            .watch
            .as_deref()
            .map(|watch| format!("│ {watch} "))
            .unwrap_or_default();
        format!(
            "{} │ {} │ ↑{} ↓{} {}{}{}{}│ {}",
            self.cwd_short,
            self.model,
            self.tokens_in,
            self.tokens_out,
            context,
            tps,
            think,
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

    #[test]
    fn telemetry_renders_tps_and_thinking_pct_when_set() {
        let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
        f.set_decode_tps(Some(62.4));
        f.set_thinking_pct(Some(45));
        let rendered = f.render();
        assert!(rendered.contains("62.4 t/s"), "{rendered}");
        assert!(rendered.contains("think 45%"), "{rendered}");
        f.clear_telemetry();
        let cleared = f.render();
        assert!(!cleared.contains("t/s"));
        assert!(!cleared.contains("think"));
    }

    #[test]
    fn set_context_clamps_above_100_without_u8_wrapping() {
        let mut f = Footer::new(Path::new("/home/user/Projects/ganggang"), "qwen3.8");
        f.set_context(400_000, 100_000);
        assert!(f.render().contains("ctx 100%"), "{}", f.render());
    }
}
