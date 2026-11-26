//! `bash`: run a shell command, capture everything, report the exit code.
//!
//! Runs with the user's permissions in the session cwd — the documented
//! trust boundary (spec §10). Everything that can hang has a number: the
//! per-call timeout is clamped to the configured maximum. And everything
//! that can hang can also be stopped: the user's abort takes down the
//! whole process group, the same kill the timeout uses.
//!
//! Every command also carries the run's nest ([`crate::nested`]): the
//! limits and lineage of a `gg -p` the command starts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, parse_args};
use crate::nested::{self, ParentEnv, SharedParentEnv};

mod hint;
mod output;
mod process;

use output::{OUTPUT_CAP, render_capture};
use process::{Stop, drain_and_wait, settle, spawn};

pub struct BashTool {
    cwd: PathBuf,
    max_timeout_secs: u64,
    /// The nest shared with the run: what a `gg -p` this tool starts
    /// inherits. Read per command, so a model switch shows at once.
    nest: SharedParentEnv,
}

impl BashTool {
    pub fn new(cwd: PathBuf, max_timeout_secs: u64, nest: SharedParentEnv) -> Self {
        Self {
            cwd,
            max_timeout_secs,
            nest,
        }
    }
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    timeout_secs: Option<u64>,
}

/// Outcome of a bounded bash run. Shared by the bash tool and the TUI's
/// `!` shell — one spawn path, one kill discipline (an orphaned process
/// group is an orphaned process group whoever asked for the command).
#[derive(Debug)]
pub(crate) enum BashOutcome {
    /// Ran to completion; `output` keeps the head and tail of the stream
    /// within `cap` bytes, with a visible truncation marker.
    Done {
        output: String,
        status: std::process::ExitStatus,
    },
    /// Killed at the deadline, whole process group included. `output` is
    /// whatever arrived before the kill.
    TimedOut { output: String },
    /// Killed because the user stopped the run, whole process group
    /// included. `output` is whatever arrived before the kill.
    Cancelled { output: String },
    /// Never started or unwaitable; the string is a user-facing message.
    Failed(String),
}

/// Run `bash -c command` in `cwd`: streams merged inside the shell, head
/// and tail kept within `cap` bytes and the rest drained (bounded memory,
/// real exit code), process-group kill on timeout **and** on cancellation.
/// `nest` is the run's, for the nested runs the command may start; `None`
/// leaves the command's environment alone.
pub(crate) async fn run_bash(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cap: usize,
    cancel: &Cancel,
    nest: Option<&ParentEnv>,
) -> BashOutcome {
    let spawned = match spawn(command, cwd, nest) {
        Ok(spawned) => spawned,
        Err(e) => return BashOutcome::Failed(format!("failed to spawn bash: {e}")),
    };
    let pgid = spawned.pgid;
    let capture = Arc::clone(&spawned.capture);
    let status = match settle(drain_and_wait(spawned, cap), timeout, cancel, pgid).await {
        Ok(status) => status,
        Err(stop) => {
            let output = render_capture(&capture, cap);
            return match stop {
                Stop::TimedOut => BashOutcome::TimedOut { output },
                Stop::Cancelled => BashOutcome::Cancelled { output },
                Stop::Failed(message) => BashOutcome::Failed(message),
            };
        }
    };

    BashOutcome::Done {
        output: render_capture(&capture, cap),
        status,
    }
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a shell command in bash; returns merged stdout+stderr and the exit code. \
 Output keeps the start and end when over 10 KiB. Nonzero exits are errors that still \
 include the output; a timeout or abort reports any partial output. A `gg -p` started \
 here is a nested run: it keeps this run's tools, hooks, model and limits."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to run"},
                "timeout_secs": {"type": "integer", "description": "Kill the command after this many seconds"}
            },
            "required": ["command"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: BashArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let timeout = Duration::from_secs(
                args.timeout_secs
                    .unwrap_or(self.max_timeout_secs)
                    .min(self.max_timeout_secs),
            );
            let nest = nested::current(&self.nest);
            match run_bash(
                &args.command,
                &self.cwd,
                timeout,
                OUTPUT_CAP,
                cancel,
                Some(&nest),
            )
            .await
            {
                BashOutcome::Done { output, status } => {
                    let code = match status.code() {
                        Some(c) => c.to_string(),
                        None => "killed by signal".to_string(),
                    };
                    let mut result = format!("[exit code: {code}]\n{output}");
                    // A shell command that duplicates a native tool gets a
                    // one-line nudge; the command itself ran regardless.
                    if let Some(h) = hint::workaround_hint(&args.command, &self.cwd) {
                        if !result.ends_with('\n') {
                            result.push('\n');
                        }
                        result.push_str(&format!("[hint: {h}]"));
                    }
                    if status.success() {
                        ToolResult::ok(result)
                    } else {
                        // Nonzero exit is an error *result* that still
                        // carries the output — the model needs to see what
                        // went wrong to retry.
                        ToolResult::err(result)
                    }
                }
                BashOutcome::TimedOut { output } => ToolResult::err(with_partial(
                    format!(
                        "command timed out after {}s and was killed",
                        timeout.as_secs()
                    ),
                    output,
                )),
                // Distinct from a timeout: the user stopped this one, and
                // the wording must not read like the command was too slow.
                BashOutcome::Cancelled { output } => ToolResult::err(with_partial(
                    "command aborted by user and killed".to_string(),
                    output,
                )),
                BashOutcome::Failed(message) => ToolResult::err(message),
            }
        })
    }
}

/// Hand a command the nest it may start. Every variable is either set or
/// removed: one left in place would pass this run's inherited values off
/// as its own.
fn apply_env(command: &mut tokio::process::Command, exports: Vec<(&'static str, Option<String>)>) {
    for (name, value) in exports {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
}

/// Attach whatever arrived before a kill, leaving the bare message exact
/// when nothing did.
fn with_partial(message: String, output: String) -> String {
    if output.is_empty() {
        message
    } else {
        format!("{message}\n[partial output]\n{output}")
    }
}

#[cfg(test)]
mod tests;
