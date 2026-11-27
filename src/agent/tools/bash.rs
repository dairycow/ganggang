//! `bash`: run a shell command, capture everything, report the exit code.
//!
//! Runs with the user's permissions in the session cwd — the documented
//! trust boundary (spec §10). Everything that can hang has a number: the
//! per-call timeout is clamped to the configured maximum. And everything
//! that can hang can also be stopped: the user's abort takes down the
//! whole process group, the same kill the timeout uses.
//!
//! Every command also carries the run's nest ([`crate::nested`]): the
//! limits and lineage of a `gg -p` the command starts. And every command
//! carries `GG_JOB_LOG`, the run's job log ([`joblog`]): a detached job
//! the command starts registers there, and `wait_job` waits for it by
//! name, past the per-call ceiling.
//!
//! A command longer than the deadline can be kept as a background job
//! instead of being killed ([`jobs`]); every call still ends in time, and
//! a kept job dies with the process, even at an exit that skips
//! destructors.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, parse_args};
use crate::nested::{self, ParentEnv, SharedParentEnv};

mod hint;
mod joblog;
mod jobs;
mod output;
mod process;

pub(crate) use jobs::kill_jobs_at_exit;

use joblog::JobLog;
use output::{OUTPUT_CAP, render_capture};
use process::{Stop, drain_and_wait, settle, spawn};

pub struct BashTool {
    cwd: PathBuf,
    max_timeout_secs: u64,
    /// The nest shared with the run: what a `gg -p` this tool starts
    /// inherits. Read per command, so a model switch shows at once.
    nest: SharedParentEnv,
    /// Commands kept past their deadline by `background: true`, numbered
    /// for `wait_for` and `stop`.
    jobs: jobs::Jobs,
    /// The run's job log: `$GG_JOB_LOG` on every command, where a
    /// detached job a command starts registers itself for `wait_job`.
    joblog: JobLog,
}

impl BashTool {
    pub fn new(cwd: PathBuf, max_timeout_secs: u64, nest: SharedParentEnv) -> Self {
        Self {
            cwd,
            max_timeout_secs,
            nest,
            jobs: jobs::Jobs::default(),
            joblog: JobLog::create(),
        }
    }
}

#[derive(Deserialize)]
struct BashArgs {
    /// Absent for a `wait_for` or `stop` call, which acts on a job instead.
    command: Option<String>,
    timeout_secs: Option<u64>,
    /// Keep the command as a job when the deadline passes, not kill it.
    background: Option<bool>,
    /// Collect this background job instead of running a command.
    wait_for: Option<u64>,
    /// Wait for the detached job registered under this name in the run's
    /// job log (`$GG_JOB_LOG`), instead of running a command.
    wait_job: Option<String>,
    /// Kill this background job instead of running a command.
    stop: Option<u64>,
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
    run_logged(command, cwd, timeout, cap, cancel, nest, None).await
}

/// [`run_bash`] for the tool's own commands: the command also carries the
/// run's job log, so a detached job it starts can register in it.
pub(crate) async fn run_bash_logged(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cap: usize,
    cancel: &Cancel,
    nest: Option<&ParentEnv>,
    job_log: &Path,
) -> BashOutcome {
    run_logged(command, cwd, timeout, cap, cancel, nest, Some(job_log)).await
}

/// The one spawn path behind both: `job_log` is the run's log when the
/// command is the bash tool's own, `None` for a caller that has none.
async fn run_logged(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cap: usize,
    cancel: &Cancel,
    nest: Option<&ParentEnv>,
    job_log: Option<&Path>,
) -> BashOutcome {
    let spawned = match spawn(command, cwd, nest, job_log) {
        Ok(spawned) => spawned,
        Err(e) => return BashOutcome::Failed(format!("failed to spawn bash: {e}")),
    };
    let pgid = spawned.pgid;
    let capture = Arc::clone(&spawned.capture);
    let status = match settle(drain_and_wait(spawned, cap, || {}), timeout, cancel, pgid).await {
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

    fn jobs_report(&self) -> Option<String> {
        let mut lines = self.jobs.report();
        lines.extend(joblog::report(&self.joblog));
        Some(if lines.is_empty() {
            "no jobs".to_string()
        } else {
            lines.join("\n")
        })
    }

    fn description(&self) -> &'static str {
        "Run a shell command in bash; returns merged stdout+stderr and the exit code. \
 Output keeps the start and end when over 10 KiB. Nonzero exits are errors that still \
 include the output; a timeout or abort reports any partial output. A command that may \
 outlive the timeout takes background: true: it keeps running as a job and the result \
 names it; wait_for: <id> waits for that job and returns its output; stop: <id> kills \
 it. wait_job: <name> waits for a detached job the command registered in the run's job \
 log ($GG_JOB_LOG), with a wait budget of its own. A `gg -p` started \
 here is a nested run: it keeps this run's tools, hooks, model and limits."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to run (required unless wait_for, wait_job or stop is set)"},
                "timeout_secs": {"type": "integer", "description": format!(
                    "Kill the command after this many seconds; the most this session allows is {}. \
                      A command always keeps that ceiling, and wait_for waits it by default. A wait \
                      (wait_for or wait_job) may pass a budget past the ceiling: wait_job defaults \
                      to 600 s, both clamp to {} s",
                    self.max_timeout_secs,
                    joblog::WAIT_BUDGET_MAX_SECS
                )},
                "background": {"type": "boolean", "description": "For a command that may still be running at the deadline: keep it running as a job and name it in the result instead of killing it. Collect the job later with wait_for"},
                "wait_for": {"type": "integer", "description": "Wait for background job <id> and return its output and exit code. A job still running when the budget passes is reported with its output so far. Takes no command"},
                "wait_job": {"type": "string", "description": "Wait for the detached job a command registered under this name in the run's job log ($GG_JOB_LOG): a start record (name, pid, directory) when it started, a done record (name, exit, note) when it ended. Waits until the done record lands or the pid is gone; a job still running when the budget passes is reported with its progress so far. Takes no command"},
                "stop": {"type": "integer", "description": "Kill background job <id>, whole process group included. Takes no command"}
            }
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: BashArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let nest = nested::current(&self.nest);
            let job_args = args.wait_for.is_some() as u8
                + args.stop.is_some() as u8
                + args.wait_job.is_some() as u8;
            if job_args > 1 || job_args == 1 && args.command.is_some() {
                return ToolResult::err(jobs::JOB_TAKES_NO_COMMAND.to_string());
            }
            if let Some(id) = args.wait_for {
                // A wait may outlast the bash ceiling; a command cannot.
                let budget = joblog::wait_budget(args.timeout_secs, self.max_timeout_secs);
                return self.jobs.collect(id, budget, cancel).await;
            }
            if let Some(name) = args.wait_job.as_deref() {
                let budget = joblog::wait_budget(args.timeout_secs, joblog::WAIT_JOB_DEFAULT_SECS);
                return joblog::wait(&self.joblog, name, budget, cancel).await;
            }
            if let Some(id) = args.stop {
                return self.jobs.stop(id, cancel).await;
            }
            let Some(command) = args.command else {
                return ToolResult::err(jobs::NO_COMMAND.to_string());
            };
            let timeout = Duration::from_secs(
                args.timeout_secs
                    .unwrap_or(self.max_timeout_secs)
                    .min(self.max_timeout_secs),
            );
            let outcome = if args.background == Some(true) {
                match self
                    .jobs
                    .start(
                        &command,
                        &self.cwd,
                        timeout,
                        cancel,
                        Some(&nest),
                        Some(self.joblog.path()),
                    )
                    .await
                {
                    jobs::Start::Settled(outcome) => outcome,
                    jobs::Start::Kept { id, output } => {
                        return ToolResult::ok(jobs::kept(id, timeout, output));
                    }
                }
            } else {
                run_bash_logged(
                    &command,
                    &self.cwd,
                    timeout,
                    OUTPUT_CAP,
                    cancel,
                    Some(&nest),
                    self.joblog.path(),
                )
                .await
            };
            match outcome {
                BashOutcome::Done { output, status } => {
                    let mut result = done_result(output, status);
                    // A shell command that duplicates a native tool gets a
                    // one-line nudge; the command itself ran regardless.
                    if let Some(h) = hint::workaround_hint(&command, &self.cwd) {
                        if !result.output.ends_with('\n') {
                            result.output.push('\n');
                        }
                        result.output.push_str(&format!("[hint: {h}]"));
                    }
                    result
                }
                BashOutcome::TimedOut { output } => ToolResult::err(with_partial(
                    format!(
                        "command timed out after {}s and was killed{}",
                        timeout.as_secs(),
                        jobs::NEEDS_BACKGROUND
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

/// What a finished command reports, whether it ran inside the call or was
/// collected from a job: the exit code first, then everything it printed.
fn done_result(output: String, status: std::process::ExitStatus) -> ToolResult {
    let code = match status.code() {
        Some(c) => c.to_string(),
        None => "killed by signal".to_string(),
    };
    let result = format!("[exit code: {code}]\n{output}");
    if status.success() {
        ToolResult::ok(result)
    } else {
        // Nonzero exit is an error *result* that still carries the output —
        // the model needs to see what went wrong to retry.
        ToolResult::err(result)
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
