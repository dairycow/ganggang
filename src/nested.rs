//! Nested runs: a `gg` that another `gg`'s `bash` tool started.
//!
//! Delegation is headless. A run hands a bounded piece of work to `gg -p`
//! through `bash`, so the `bash` tool marks every command it runs with a
//! few variables describing the run ([`ParentEnv`]). A headless boot that
//! finds [`DEPTH`] among them is a nested run ([`NestedBoot`]) and clamps
//! itself to the run that started it:
//!
//! - tools: a subset of the parent's allowlist;
//! - turns: at most the parent's `max_turns`;
//! - hooks: the parent's, on or off; `--no-hooks` is ignored;
//! - model: the parent's current model unless `-m` or `-P` names one;
//! - depth: one level, so a nested run cannot start a nested run;
//! - session: a fresh session's header records the parent's session id.
//!
//! A TUI boot ignores the variables: a person is driving it. The variables
//! are accident-proofing rather than a boundary, since a command can unset
//! them.

use std::sync::{Arc, RwLock};

use anyhow::Context as _;

/// Marks a nested run: how many runs sit above it.
pub const DEPTH: &str = "GG_NEST_DEPTH";
/// The parent run's session id, when it keeps a session.
pub const PARENT_SESSION: &str = "GG_PARENT_SESSION";
/// The parent run's current model, as a `provider/model` specification.
pub const PARENT_MODEL: &str = "GG_PARENT_MODEL";
/// The parent run's tool allowlist: the most a nested run may have.
pub const TOOL_CAP: &str = "GG_TOOL_CAP";
/// The parent run's `max_turns`, when it has one.
pub const MAX_TURNS_CAP: &str = "GG_MAX_TURNS_CAP";
/// `1` when the parent run's hooks are off.
pub const NO_HOOKS: &str = "GG_NO_HOOKS";

/// Runs may nest this deep: a run, and the runs it starts.
pub const MAX_DEPTH: u32 = 1;

/// A nest shared between a run and its `bash` tool. The run writes its
/// current model; the tool reads a snapshot per command.
pub type SharedParentEnv = Arc<RwLock<ParentEnv>>;

/// What a run tells the commands its `bash` tool runs about itself: the
/// nest they may start. `depth` is the run's own, 0 for a run a person
/// started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentEnv {
    pub depth: u32,
    /// The run's session id; `None` under `--no-session`.
    pub session: Option<String>,
    /// The run's current model, as `Endpoint::label()` writes it.
    pub model: Option<String>,
    /// The run's effective tool allowlist; `None` when none was resolved.
    pub tool_cap: Option<Vec<String>>,
    /// The run's effective `max_turns`.
    pub max_turns_cap: Option<u32>,
    /// Whether the run's hooks are off.
    pub hooks_off: bool,
}

impl ParentEnv {
    /// A shared cell a run and its `bash` tool both hold.
    pub fn shared(self) -> SharedParentEnv {
        Arc::new(RwLock::new(self))
    }

    /// The variables set on every command the run's `bash` tool starts.
    /// `None` removes a variable, so a value this run inherited from its
    /// own parent cannot pass for this run's.
    pub fn exports(&self) -> Vec<(&'static str, Option<String>)> {
        vec![
            (DEPTH, Some(self.depth.saturating_add(1).to_string())),
            (PARENT_SESSION, non_empty(self.session.clone())),
            (PARENT_MODEL, non_empty(self.model.clone())),
            // No cap, no variable.
            (
                TOOL_CAP,
                self.tool_cap.as_ref().map(|tools| tools.join(",")),
            ),
            (MAX_TURNS_CAP, self.max_turns_cap.map(|n| n.to_string())),
            (NO_HOOKS, self.hooks_off.then(|| "1".to_string())),
        ]
    }
}

/// The nest as it stands now. A holder that panicked while writing must
/// not stop commands from running, so a poisoned lock yields its value.
pub fn current(nest: &SharedParentEnv) -> ParentEnv {
    match nest.read() {
        Ok(env) => env.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Record the run's current model, for the runs its commands start.
pub fn set_model(nest: &SharedParentEnv, model: String) {
    match nest.write() {
        Ok(mut env) => env.model = Some(model),
        Err(poisoned) => poisoned.into_inner().model = Some(model),
    }
}

/// A nested boot: what a headless `gg` reads from the variables a parent
/// run exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedBoot {
    pub depth: u32,
    pub parent_session: Option<String>,
    pub parent_model: Option<String>,
    /// Empty when the parent allows no tools at all.
    pub tool_cap: Vec<String>,
    pub max_turns_cap: Option<u32>,
    pub hooks_off: bool,
}

impl NestedBoot {
    /// Read the nest from this process's environment.
    pub fn from_process_env() -> anyhow::Result<Option<Self>> {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// Read the nest through `get`. `Ok(None)` when [`DEPTH`] is absent or
    /// blank: not a nested run, whatever else is set.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let Some(raw) = non_empty(get(DEPTH)) else {
            return Ok(None);
        };
        let depth = raw
            .parse::<u32>()
            .ok()
            .filter(|depth| *depth >= 1)
            .with_context(|| format!("{DEPTH} must be a positive integer, got {raw:?}"))?;
        anyhow::ensure!(
            depth <= MAX_DEPTH,
            "this run is already nested {depth} level(s) deep: \
             a nested run cannot start a nested run"
        );
        let max_turns_cap = match non_empty(get(MAX_TURNS_CAP)) {
            None => None,
            Some(raw) => Some(raw.parse::<u32>().with_context(|| {
                format!("{MAX_TURNS_CAP} must be a non-negative integer, got {raw:?}")
            })?)
            .filter(|turns| *turns > 0),
        };
        Ok(Some(Self {
            depth,
            parent_session: non_empty(get(PARENT_SESSION)),
            parent_model: non_empty(get(PARENT_MODEL)),
            tool_cap: split_tools(get(TOOL_CAP).as_deref()),
            max_turns_cap,
            hooks_off: get(NO_HOOKS).as_deref() == Some("1"),
        }))
    }

    /// The tool allowlist a nested run boots with. `requested` is its own
    /// `--tools` flag and `resolved` what that flag or the config resolved
    /// to. A named tool outside the parent's allowlist fails the boot: the
    /// run asked for something it cannot have. Without a flag, the resolved
    /// list keeps only the parent's tools.
    pub fn narrow_tools(
        &self,
        requested: Option<&[String]>,
        resolved: Vec<String>,
    ) -> anyhow::Result<Vec<String>> {
        if let Some(requested) = requested {
            for name in requested {
                if !self.tool_cap.contains(name) {
                    anyhow::bail!("--tools {name} is outside the parent run's allowlist");
                }
            }
            return Ok(resolved);
        }
        Ok(resolved
            .into_iter()
            .filter(|name| self.tool_cap.contains(name))
            .collect())
    }

    /// The turn budget of a nested run: its own `max_turns`, lowered to the
    /// parent's when the parent has one.
    pub fn effective_max_turns(&self, max_turns: Option<u32>) -> Option<u32> {
        match (max_turns, self.max_turns_cap) {
            (Some(turns), Some(cap)) => Some(turns.min(cap)),
            (None, Some(cap)) => Some(cap),
            (other, None) => other,
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// A comma-separated allowlist: trimmed, without blanks or repeats, in the
/// order written.
fn split_tools(raw: Option<&str>) -> Vec<String> {
    let mut tools = Vec::new();
    for name in raw.unwrap_or_default().split(',') {
        let name = name.trim();
        if !name.is_empty() && !tools.iter().any(|seen: &String| seen == name) {
            tools.push(name.to_string());
        }
    }
    tools
}

#[cfg(test)]
mod tests;
