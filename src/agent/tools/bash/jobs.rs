//! Background jobs: a command that outlives the call that started it.
//!
//! `bash_timeout_secs` bounds every [`bash`](super) call, so a command
//! longer than that ceiling could only be started by the model itself, in
//! the shell, and polled with sleeps. `background: true` is the harness's
//! way. The call waits for the command as any call does, and a command
//! still running at the deadline is kept, as a numbered job, instead of
//! killed. `wait_for: <id>` waits for a job and returns what a finished
//! command does; `stop: <id>` kills one.
//!
//! A job lives as long as the tool: when the table is dropped, every job
//! still running goes down with it, whole process group included. The same
//! kill rides along on an exit that skips destructors
//! ([`kill_jobs_at_exit`]), so a kept job does not outlive the process
//! either.

use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::output::{OUTPUT_CAP, SharedCapture, render_capture};
use super::process::{Spawned, drain_and_wait, kill_group, spawn, stop_requested};
use super::{BashOutcome, Cancel, done_result, with_partial};
use crate::agent::tools::ToolResult;
use crate::nested::ParentEnv;

/// What a call with `wait_for` or `stop` and a `command` is told.
pub(super) const JOB_TAKES_NO_COMMAND: &str =
    "wait_for and stop take a job's number and no command, and not each other";
/// What a call with neither is told.
pub(super) const NO_COMMAND: &str = "bash needs a command, or wait_for or stop with a job's number";
/// Appended to a timeout: the one place a model that did not read the schema
/// learns that the deadline is final, and what to do about it.
pub(super) const NEEDS_BACKGROUND: &str = "; a longer command needs background: true";

/// The jobs a tool has kept, numbered from 1 in the order they were kept.
#[derive(Default)]
pub(super) struct Jobs(Mutex<Vec<Arc<Job>>>);

/// What the task draining a command answers with once it has exited: its
/// status, or the error that ended the wait on it. Every waiter reads the
/// slot, so the error is text rather than an `io::Error`, which cannot be
/// cloned.
type Settled = watch::Receiver<Option<std::result::Result<ExitStatus, String>>>;

/// One command kept past its deadline.
struct Job {
    capture: SharedCapture,
    pgid: Option<u32>,
    /// Set once the shell has been reaped. Until then the group id is
    /// pinned to this job; after it the number may belong to another
    /// process, so the group is no longer signalled.
    exited: watch::Receiver<bool>,
    /// `None` until the command has exited and its pipe is drained.
    settled: Settled,
    /// The task draining the command; aborted when a kill leaves it nothing
    /// to wait for.
    runner: tokio::task::AbortHandle,
}

/// How starting a job ended.
pub(super) enum Start {
    /// The command finished, failed or was stopped inside the call: what a
    /// foreground run would report.
    Settled(BashOutcome),
    /// The deadline passed with the command running: it is kept as job
    /// `id`, and `output` is what it printed until then.
    Kept { id: u64, output: String },
}

impl Jobs {
    /// Run `command` as a job: wait for it up to `timeout`, as for any
    /// command, and keep it when the deadline passes. A stop request inside
    /// that first wait kills it as it would any command.
    pub(super) async fn start(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
        cancel: &Cancel,
        nest: Option<&ParentEnv>,
    ) -> Start {
        let spawned = match spawn(command, cwd, nest) {
            Ok(spawned) => spawned,
            Err(e) => {
                return Start::Settled(BashOutcome::Failed(format!("failed to spawn bash: {e}")));
            }
        };
        let job = Arc::new(Job::running(spawned));
        tokio::select! {
            biased;
            result = Arc::clone(&job).finished() => Start::Settled(match result {
                Ok(status) => BashOutcome::Done {
                    output: render_capture(&job.capture, OUTPUT_CAP),
                    status,
                },
                Err(e) => BashOutcome::Failed(format!("waited on bash: {e}")),
            }),
            () = stop_requested(cancel) => {
                job.kill_group().await;
                // Dropping the runner drops the child, whose `kill_on_drop`
                // is the backstop when the group signal missed.
                job.runner.abort();
                Start::Settled(BashOutcome::Cancelled {
                    output: render_capture(&job.capture, OUTPUT_CAP),
                })
            }
            () = tokio::time::sleep(timeout) => Start::Kept {
                id: self.keep(Arc::clone(&job)),
                output: render_capture(&job.capture, OUTPUT_CAP),
            },
        }
    }

    /// `wait_for: id`: wait up to `timeout` for job `id` and return what a
    /// finished command does. A job still running at the deadline is
    /// reported with what it has printed, and stays kept. A stop request
    /// ends the wait, not the job.
    pub(super) async fn collect(&self, id: u64, timeout: Duration, cancel: &Cancel) -> ToolResult {
        let Some(job) = self.get(id) else {
            return ToolResult::err(unknown(id));
        };
        tokio::select! {
            biased;
            result = Arc::clone(&job).finished() => match result {
                Ok(status) => done_result(render_capture(&job.capture, OUTPUT_CAP), status),
                Err(e) => ToolResult::err(format!("waited on background job {id}: {e}")),
            },
            () = stop_requested(cancel) => ToolResult::err(format!(
                "wait for background job {id} aborted by user; the job keeps running"
            )),
            () = tokio::time::sleep(timeout) => ToolResult::ok(with_partial(
                format!(
                    "background job {id} is still running after {secs}s. Call bash with \
                     wait_for: {id} to keep waiting or stop: {id} to kill it; a job that \
                     never exits, such as a server, is read this way too",
                    secs = timeout.as_secs(),
                ),
                render_capture(&job.capture, OUTPUT_CAP),
            )),
        }
    }

    /// `stop: id`: kill job `id`, whole process group included, and wait
    /// for it to be reaped. A job that had already exited is left as it is,
    /// so its output can still be collected. A job whose shell has exited
    /// is not signalled: see [`Job::kill_group`].
    pub(super) async fn stop(&self, id: u64, cancel: &Cancel) -> ToolResult {
        let Some(job) = self.get(id) else {
            return ToolResult::err(unknown(id));
        };
        if job.settled.borrow().is_some() {
            return ToolResult::ok(format!(
                "background job {id} had already exited; wait_for: {id} returns its output"
            ));
        }
        if !job.shell_alive() {
            return ToolResult::ok(format!(
                "background job {id}: its shell has already exited, so there is no group \
                 to kill; a process the command left behind still holds its output. \
                 wait_for: {id} returns what it printed once that process exits"
            ));
        }
        job.kill_group().await;
        tokio::select! {
            biased;
            // SIGKILL closes every write end in the group, so the pipe
            // reaches EOF and the runner answers. The bound covers a kill
            // that failed, or a member stuck in uninterruptible sleep.
            () = tokio::time::sleep(Duration::from_secs(5)) => ToolResult::ok(format!(
                "background job {id} was sent SIGKILL but had not exited after 5s; \
                 wait_for: {id} shows what it printed"
            )),
            () = stop_requested(cancel) => ToolResult::err(format!(
                "stop of background job {id} aborted by user; the kill was sent"
            )),
            _ = Arc::clone(&job).finished() => ToolResult::ok(format!(
                "background job {id} stopped; wait_for: {id} returns what it printed"
            )),
        }
    }

    /// Keep a job still running and give it its number. The exit registry
    /// gets a handle too, so an exit that skips [`Drop`] can still kill it.
    fn keep(&self, job: Arc<Job>) -> u64 {
        register_for_exit(&job);
        self.with(|jobs| {
            jobs.push(job);
            jobs.len() as u64
        })
    }

    /// Job number `id`, numbered from 1.
    fn get(&self, id: u64) -> Option<Arc<Job>> {
        id.checked_sub(1)
            .and_then(|at| usize::try_from(at).ok())
            .and_then(|at| self.with(|jobs| jobs.get(at).cloned()))
    }

    /// The table itself. A lock a panicked thread left behind still holds
    /// the jobs, so the poison is cleared rather than obeyed.
    fn with<T>(&self, visit: impl FnOnce(&mut Vec<Arc<Job>>) -> T) -> T {
        let mut jobs = match self.0.lock() {
            Ok(jobs) => jobs,
            Err(poisoned) => poisoned.into_inner(),
        };
        visit(&mut jobs)
    }
}

/// What both `wait_for` and `stop` say about a number no job has.
fn unknown(id: u64) -> String {
    format!("no background job {id}: jobs are numbered from 1 as bash keeps them")
}

impl Job {
    /// Hand a started command to its own task, so the call that waits on
    /// it may end first; `settled` is what that task answers, and `exited`
    /// is set as soon as the shell is reaped.
    fn running(spawned: Spawned) -> Self {
        let (answer, settled) = watch::channel(None);
        let (shell_exited, exited) = watch::channel(false);
        Self {
            capture: Arc::clone(&spawned.capture),
            pgid: spawned.pgid,
            exited,
            settled,
            runner: tokio::spawn(async move {
                let status = drain_and_wait(spawned, OUTPUT_CAP, || {
                    shell_exited.send_replace(true);
                })
                .await
                .map_err(|e| e.to_string());
                answer.send_modify(|slot| *slot = Some(status));
            })
            .abort_handle(),
        }
    }

    /// Whether the shell is alive or unreaped. While it is, no other
    /// process can hold this job's group id.
    fn shell_alive(&self) -> bool {
        !*self.exited.borrow()
    }

    /// Kill the whole process group, but only while [`Self::shell_alive`]
    /// holds. A job also counts as unsettled while a process the command
    /// left behind holds its pipe, long after the shell is gone; by then the
    /// group id may have been given to an unrelated process group.
    async fn kill_group(&self) {
        if self.shell_alive() {
            kill_group(self.pgid).await;
        }
    }

    /// Resolves once the command has exited and its pipe is drained. The
    /// task draining it always answers; an empty answer means that task
    /// was dropped first, as when the runtime shuts down.
    async fn finished(self: Arc<Self>) -> std::result::Result<ExitStatus, String> {
        let mut settled = self.settled.clone();
        settled
            .wait_for(|settled| settled.is_some())
            .await
            .ok()
            .and_then(|settled| (*settled).clone())
            .unwrap_or_else(|| Err("the job's runner was dropped".to_string()))
    }
}

/// Every job this process has kept, so that an exit which runs no
/// destructors can still kill one. Entries whose command has settled are
/// pruned whenever a new job arrives; the kill itself re-checks every
/// entry, so a stale one is skipped rather than trusted.
static KEPT_JOBS: Mutex<Vec<Arc<Job>>> = Mutex::new(Vec::new());

/// The blocking form of the deadline kill over `jobs`: every job whose
/// command has not settled and whose shell is still alive goes down with
/// its whole process group. Answers with the number of groups it sent a
/// kill to.
fn kill_live_groups(jobs: &[Arc<Job>]) -> usize {
    let mut killed = 0;
    for job in jobs
        .iter()
        .filter(|job| job.settled.borrow().is_none() && job.shell_alive())
    {
        if let Some(pgid) = job.pgid {
            let _ = std::process::Command::new("kill")
                .args(["-9", &format!("-{pgid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            killed += 1;
        }
    }
    killed
}

/// Add a kept job to [`KEPT_JOBS`].
fn register_for_exit(job: &Arc<Job>) {
    let mut kept = match KEPT_JOBS.lock() {
        Ok(kept) => kept,
        Err(poisoned) => poisoned.into_inner(),
    };
    kept.retain(|kept| kept.settled.borrow().is_none());
    kept.push(Arc::clone(job));
}

/// Kill every kept job still running, group included, and forget them all.
/// `process::exit` runs no destructors, so the exit paths in `main` call
/// this where [`Drop`] would never run; a normal exit needs it not.
pub(crate) fn kill_jobs_at_exit() -> usize {
    let mut kept = match KEPT_JOBS.lock() {
        Ok(kept) => kept,
        Err(poisoned) => poisoned.into_inner(),
    };
    let killed = kill_live_groups(&kept);
    kept.clear();
    killed
}

impl Drop for Jobs {
    /// A job outlives its calls, not the tool. Nothing can be awaited here,
    /// so this is the blocking form of the group kill every deadline uses.
    fn drop(&mut self) {
        self.with(|jobs| {
            kill_live_groups(jobs);
        });
    }
}

/// The result of a call that kept its command as job `id` after waiting
/// `waited` for it.
pub(super) fn kept(id: u64, waited: Duration, output: String) -> String {
    with_partial(
        format!(
            "still running after {secs}s, kept as background job {id}: call bash with \
             wait_for: {id} to collect its output and exit code, or stop: {id} to kill it",
            secs = waited.as_secs(),
        ),
        output,
    )
}
