//! User-level hooks (spec §22): one-shot shell commands at the loop's
//! interception points.
//!
//! A hook is not a plugin: it is a process, given one JSON object on stdin,
//! that may print a deny decision for `pre_tool`. Failures are warnings and
//! deliberately fail open — a broken guard must not wedge the agent. Hooks
//! are read from `~/.gg/hooks.toml` only; a project file is ignored so a
//! cloned repository cannot add executable behavior.

mod invoke;
mod payload;
mod report;
mod run;

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

const DEFAULT_TIMEOUT_SECS: u64 = 10;
const REASON_CAP: usize = 2 * 1024;

/// The loop moments a hook can observe (spec §22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a tool call; may deny it.
    PreTool,
    /// After a tool result is recorded; observe only.
    PostTool,
    /// Once after a clean run; observe only.
    RunEnd,
}

impl HookEvent {
    /// The config/JSON spelling (`pre_tool`, `post_tool`, `run_end`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreTool => "pre_tool",
            Self::PostTool => "post_tool",
            Self::RunEnd => "run_end",
        }
    }
}

/// One `[[hooks]]` entry. `tools` is `None` when omitted (all tools);
/// an explicitly empty list is rejected by [`validate`] — the omission
/// already means "all", so an empty list can only be a mistake.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Hook {
    /// Which loop moment this hook observes.
    pub event: HookEvent,
    /// Exact tool names; `None` (omitted) matches every tool. `Some([])` is
    /// rejected; `["*"]` is the explicit spelling of "all".
    pub tools: Option<Vec<String>>,
    /// Shell command, run as `bash -c` in the session cwd.
    pub command: String,
    /// Deadline for the whole invocation; the process group is killed.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Hook {
    fn matches(&self, tool: &str) -> bool {
        self.tools
            .as_ref()
            .is_none_or(|names| names.iter().any(|n| n == "*" || n == tool))
    }
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// The loaded hook set. Cloned per Agent; the TUI factory reuses one
/// instance across sessions.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    specs: Vec<Hook>,
    disabled: bool,
}

impl Hooks {
    #[cfg(test)]
    pub(crate) fn new(specs: Vec<Hook>) -> Self {
        Self {
            specs,
            disabled: false,
        }
    }

    /// The declared hooks, in file order (`/hooks` lists these).
    pub fn specs(&self) -> &[Hook] {
        &self.specs
    }

    /// True when `--no-hooks` skipped loading.
    pub fn disabled(&self) -> bool {
        self.disabled
    }

    /// True when no hooks are declared (or hooks are disabled).
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

/// The outcome of a `pre_tool` pass. Warnings ride with the decision so
/// the loop remains the only place that emits events.
#[derive(Debug)]
pub enum PreOutcome {
    /// No hook denied; dispatch the call.
    Allow {
        /// Failures from hooks that ran before the allow; the loop emits
        /// them as `Warned`.
        warnings: Vec<String>,
    },
    /// A hook denied the call; `reason` goes into the tool error result.
    Deny {
        /// The model-facing reason, already capped.
        reason: String,
        /// Earlier hook failures; the loop emits them first.
        warnings: Vec<String>,
    },
    /// The user's abort arrived while a hook ran; the call must not execute.
    Interrupted,
}

/// Load `~/.gg/hooks.toml` and report project-file warnings. `disabled`
/// is `--no-hooks`: no file is read and the project warning is skipped,
/// so a broken hook file cannot block the escape hatch.
pub fn load(cwd: &Path, disabled: bool) -> anyhow::Result<(Hooks, Vec<String>)> {
    let path = hooks_path();
    load_from(path.as_deref(), cwd, disabled)
}

/// Testable loader: `path` is the user file, `cwd` only locates the
/// project file that must be ignored.
pub(crate) fn load_from(
    path: Option<&Path>,
    cwd: &Path,
    disabled: bool,
) -> anyhow::Result<(Hooks, Vec<String>)> {
    if disabled {
        return Ok((
            Hooks {
                specs: Vec::new(),
                disabled: true,
            },
            Vec::new(),
        ));
    }

    let mut warnings = Vec::new();
    if cwd.join(".gg").join("hooks.toml").exists() {
        warnings.push(
            "project hooks (.gg/hooks.toml) are ignored; hooks are user-level policy \
             (~/.gg/hooks.toml)"
                .to_string(),
        );
    }

    let Some(path) = path else {
        return Ok((Hooks::default(), warnings));
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Hooks::default(), warnings));
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: FileHooks =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    validate(&file.hooks).with_context(|| format!("validating {}", path.display()))?;
    Ok((
        Hooks {
            specs: file.hooks,
            disabled: false,
        },
        warnings,
    ))
}

fn hooks_path() -> Option<PathBuf> {
    crate::home::dir().map(|home| home.join(".gg").join("hooks.toml"))
}

#[derive(Default, Deserialize)]
struct FileHooks {
    #[serde(default)]
    hooks: Vec<Hook>,
}

/// Refuse configurations that could never fire or would silently observe
/// the wrong calls. Unknown top-level keys stay ignored, like config.
fn validate(hooks: &[Hook]) -> anyhow::Result<()> {
    for (index, hook) in hooks.iter().enumerate() {
        let at = format!("hooks[{index}]");
        if hook.command.trim().is_empty() {
            anyhow::bail!("{at}.command must not be empty");
        }
        if hook.timeout_secs == 0 {
            anyhow::bail!("{at}.timeout_secs must be positive");
        }
        match hook.event {
            HookEvent::PreTool | HookEvent::PostTool => {
                if let Some(tools) = &hook.tools {
                    if tools.is_empty() {
                        anyhow::bail!(
                            "{at}.tools must be omitted or name a tool (use [\"*\"] for all)"
                        );
                    }
                    for tool in tools {
                        if tool != "*" && !crate::agent::tools::is_known_name(tool) {
                            anyhow::bail!("{at}.tools: unknown tool {tool:?}");
                        }
                    }
                }
            }
            HookEvent::RunEnd => {
                if hook.tools.is_some() {
                    anyhow::bail!("{at}.tools only applies to tool events");
                }
            }
        }
    }
    Ok(())
}

fn cap_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut cut = cap;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[truncated at {cap} bytes]", &text[..cut])
}

fn tail_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut start = text.len() - cap;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests;
