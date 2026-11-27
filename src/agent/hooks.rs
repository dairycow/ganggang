//! Hooks (spec §22): one-shot shell commands at the loop's interception
//! points.
//!
//! A hook is not a plugin: it is a process, given one JSON object on stdin,
//! that may print a deny decision for `pre_tool`. Failures are warnings and
//! deliberately fail open — a broken guard must not wedge the agent.
//!
//! Hooks come from the user's `~/.gg/hooks.toml` first, then from project
//! files: `.gg/hooks.toml` in the repo root and in each directory below it
//! down to the working directory. A project file is executable behaviour a
//! cloned repository can carry, so it loads only when its directory is one
//! the user listed in `trusted_projects` (`~/.gg/config.toml`), or lies
//! under one. Any other is left unloaded, and the loader's warning says
//! so.

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

/// The loaded hook set: the user's hooks, then the project's, each file in
/// file order. Cloned per Agent, which is built with the set loaded for
/// its directory.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    specs: Vec<Hook>,
    disabled: bool,
    /// How many of `specs` are the user file's; the rest that follow are
    /// the project files'.
    user_specs: usize,
    /// Each project hooks file found and left unloaded, with why: the
    /// loader's warnings, kept so `/hooks` can say them in the TUI, where
    /// stderr is behind the alternate screen.
    ignored: Vec<String>,
}

impl Hooks {
    #[cfg(test)]
    pub(crate) fn new(specs: Vec<Hook>) -> Self {
        Self {
            user_specs: specs.len(),
            specs,
            disabled: false,
            ignored: Vec::new(),
        }
    }

    /// A set with project hooks and unloaded project files, for `/hooks`.
    #[cfg(test)]
    pub(crate) fn with_project(user: Vec<Hook>, project: Vec<Hook>, ignored: Vec<String>) -> Self {
        Self {
            user_specs: user.len(),
            specs: user.into_iter().chain(project).collect(),
            disabled: false,
            ignored,
        }
    }

    /// The declared hooks, in file order (`/hooks` lists these).
    pub fn specs(&self) -> &[Hook] {
        &self.specs
    }

    /// Whether the hook at `index` of [`specs`](Self::specs) came from a
    /// project file rather than the user's.
    pub fn is_project(&self, index: usize) -> bool {
        index >= self.user_specs
    }

    /// The project hooks files left unloaded, each with why.
    pub fn ignored(&self) -> &[String] {
        &self.ignored
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

/// Load the hooks that govern `cwd`: `~/.gg/hooks.toml`, then every
/// trusted project file from the repo root down to `cwd`. The warnings are
/// the project files left unloaded. `disabled` is `--no-hooks`: no file is
/// read and no warning is raised, so a broken hook file cannot block the
/// escape hatch.
pub fn load(
    cwd: &Path,
    trusted: &[PathBuf],
    disabled: bool,
) -> anyhow::Result<(Hooks, Vec<String>)> {
    let path = hooks_path();
    load_from(path.as_deref(), cwd, trusted, disabled)
}

/// Testable loader: `user` is the user file, `cwd` locates the project
/// files, and `trusted` is the user's list of trusted project directories.
pub(crate) fn load_from(
    user: Option<&Path>,
    cwd: &Path,
    trusted: &[PathBuf],
    disabled: bool,
) -> anyhow::Result<(Hooks, Vec<String>)> {
    if disabled {
        return Ok((
            Hooks {
                specs: Vec::new(),
                disabled: true,
                user_specs: 0,
                ignored: Vec::new(),
            },
            Vec::new(),
        ));
    }

    let mut specs = match user {
        Some(path) => read_file(path)?,
        None => Vec::new(),
    };
    let user_specs = specs.len();
    // The user's own file can turn up as a project file: with no repo the
    // walk passes through the home directory, where it lives. It loads
    // once, as the user's.
    let user_file = user.map(real);
    let mut ignored = Vec::new();
    for (dir, path) in project_hook_files(cwd) {
        if Some(real(&path)) == user_file {
            continue;
        }
        if !is_trusted(&dir, trusted) {
            ignored.push(format!(
                "project hooks ({}) are ignored until you trust {}: add it to `trusted_projects` \
                 in ~/.gg/config.toml",
                path.display(),
                dir.display()
            ));
            continue;
        }
        specs.extend(read_file(&path)?);
    }
    Ok((
        Hooks {
            specs,
            disabled: false,
            user_specs,
            ignored: ignored.clone(),
        },
        ignored,
    ))
}

/// One hooks file's entries. A missing file declares none; one that cannot
/// be read, parsed or validated is an error naming it.
fn read_file(path: &Path) -> anyhow::Result<Vec<Hook>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: FileHooks =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    validate(&file.hooks).with_context(|| format!("validating {}", path.display()))?;
    Ok(file.hooks)
}

/// The project hooks files that govern `cwd`: `.gg/hooks.toml` in the repo
/// root and in each directory below it down to `cwd`, broadest first (the
/// walk `AGENTS.md` discovery makes; with no repo it runs to the
/// filesystem root, so outside a repo it passes through the home
/// directory, where the user's own file lives). Each is its directory
/// with its path, and only files that exist are listed;
/// [`load_from`](load_from) leaves the user's own file out of them.
fn project_hook_files(cwd: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    if let Some(root) = super::context::find_repo_root(cwd) {
        dirs.retain(|dir| dir.ancestors().any(|above| above == root));
    }
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| (dir.to_path_buf(), dir.join(".gg").join("hooks.toml")))
        .filter(|(_, path)| path.exists())
        .collect()
}

/// Whether `dir` is one of `trusted` or lies under one. Both sides are
/// canonicalised where they exist, so a symlinked listing matches its
/// target.
fn is_trusted(dir: &Path, trusted: &[PathBuf]) -> bool {
    let dir = real(dir);
    trusted
        .iter()
        .map(|root| real(root))
        .any(|root| dir.starts_with(&root))
}

fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
                        if tool != "*" && !crate::agent::tools::NAMES.contains(&tool.as_str()) {
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
