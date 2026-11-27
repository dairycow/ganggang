//! The `/jobs` view over both kinds of job: the numbered background jobs
//! a [`bash`](super) call kept (`#1 running: <command>`), and the jobs
//! commands have registered in the run's job log (`NAME · running 4m ·
//! pid 1234 · <progress>`). One line per job; [`report`] joins them,
//! and an empty world is `no jobs`.

use std::path::Path;
use std::time::Duration;

use super::joblog::{JobLog, JobState, read_all};
use super::jobs::Jobs;

/// The cap on a kept job's command in the report.
const COMMAND_CAP: usize = 80;
/// The cap on a registered job's progress line.
const PROGRESS_CAP: usize = 120;

/// `/jobs`: one line per kept background job, then one per registered
/// job; `no jobs` when the world is empty. The bash tool's
/// `jobs_report` hands this straight to the agent.
pub(super) fn report(jobs: &Jobs, log: &JobLog) -> String {
    let mut lines = kept_lines(jobs);
    lines.extend(registered_lines(log));
    if lines.is_empty() {
        "no jobs".to_string()
    } else {
        lines.join("\n")
    }
}

/// One line per kept job, in the order they were kept: the command while
/// it runs, the exit once it has one.
fn kept_lines(jobs: &Jobs) -> Vec<String> {
    jobs.with(|jobs| {
        jobs.iter()
            .enumerate()
            .map(|(at, job)| {
                let id = at + 1;
                match job.settled.borrow().as_ref() {
                    Some(Ok(status)) => format!(
                        "#{id} exited: {}",
                        status.code().map_or_else(
                            || "killed by signal".to_string(),
                            |code| format!("exit {code}"),
                        )
                    ),
                    Some(Err(e)) => format!("#{id} exited: {e}"),
                    None => format!("#{id} running: {}", clamp_chars(&job.command, COMMAND_CAP)),
                }
            })
            .collect()
    })
}

/// One line per registered job, latest record winning, in name order:
/// `NAME · running 4m · pid 1234 · <last line of NAME.err>` or
/// `NAME · done exit N · NOTE`.
fn registered_lines(log: &JobLog) -> Vec<String> {
    read_all(log)
        .into_iter()
        .filter_map(|(name, state)| match state {
            JobState::Unknown => None,
            JobState::Running { pid, dir } => {
                let age = pid_age(pid).map_or_else(
                    || "running".to_string(),
                    |age| format!("running {}", age_text(age)),
                );
                let tail = last_err_line(&dir, &name, PROGRESS_CAP)
                    .map_or_else(String::new, |line| format!(" · {line}"));
                Some(format!("{name} · {age} · pid {pid}{tail}"))
            }
            JobState::Finished { exit, note, .. } => {
                let done =
                    exit.map_or_else(|| "done".to_string(), |code| format!("done exit {code}"));
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!(" · {note}")
                };
                Some(format!("{name} · {done}{note}"))
            }
        })
        .collect()
}

/// The first `cap` characters of `text`, with an ellipsis when cut. Long
/// commands and progress lines are trimmed for `/jobs`.
pub(super) fn clamp_chars(text: &str, cap: usize) -> String {
    let mut out: String = text.chars().take(cap).collect();
    if text.chars().count() > cap {
        out.push('…');
    }
    out
}

/// The last non-empty line of `NAME.err`, trimmed to `cap` characters.
fn last_err_line(dir: &Path, name: &str, cap: usize) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(format!("{name}.err"))).ok()?;
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(clamp_chars(line, cap))
}

/// How long `pid` has been alive: the system's uptime less the process's
/// start time, both read from /proc. Linux numbers start times in fixed
/// 100 Hz ticks (`USER_HZ`). `None` off Linux, or once the process is gone.
fn pid_age(pid: u32) -> Option<Duration> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may contain spaces or a
    // closing one; every field after the last parenthesis is fixed.
    let after_comm = stat.rsplit_once(')')?.1;
    let start_ticks: f64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let uptime: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    const USER_HZ: f64 = 100.0;
    let secs = uptime - start_ticks / USER_HZ;
    if !secs.is_finite() {
        return None;
    }
    Some(Duration::from_secs_f64(secs.max(0.0)))
}

/// A duration for `/jobs`: `42s`, `4m`, then `1h3m`.
fn age_text(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
