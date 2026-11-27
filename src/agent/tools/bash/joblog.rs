//! The job log: names for the detached jobs a command starts.
//!
//! A detached job (`setsid`, a daemonised build) is invisible to the
//! numbered background jobs of [`jobs`](super::jobs): nothing in the
//! process keeps it. So every command the [`bash`](super) tool runs
//! carries `GG_JOB_LOG`, the path of a file this run owns, and a command
//! that starts a detached job registers it there, one record per line,
//! tab separated:
//!
//! ```text
//! start   NAME    PID     DIR
//! done    NAME    EXIT    NOTE
//! ```
//!
//! `DIR` holds `NAME.out`, `NAME.err` and `NAME.jsonl`; `NOTE` is one
//! line on how the job ended and may be empty. The latest record for a
//! name wins: a `done` after a `start` means finished, and a `start`
//! after a `done` means the job started again. Lines that do not parse
//! are skipped.
//!
//! [`wait`](wait) is the waiting side: `wait_job: NAME` on a `bash` call
//! polls the log until a `done` record lands or the pid is gone, for a
//! budget of its own that outlasts the session's `bash_timeout_secs`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::output::suffix_string;
use super::process::stop_requested;
use super::with_partial;
use crate::agent::tools::{Cancel, ToolResult};

/// The variable that carries the log's path onto every command.
pub(super) const JOB_LOG: &str = "GG_JOB_LOG";
/// A `wait_job` budget when the call passes no `timeout_secs`.
pub(super) const WAIT_JOB_DEFAULT_SECS: u64 = 600;
/// The most any explicit wait budget allows, `wait_for` and `wait_job`
/// alike: minutes of patience, well past the bash ceiling.
pub(super) const WAIT_BUDGET_MAX_SECS: u64 = 3600;
/// How often a wait re-reads the log.
const POLL: Duration = Duration::from_millis(250);
/// The most of `NAME.err` a failed or still-running answer carries.
const ERR_TAIL: usize = 2 * 1024;
/// The note for a pid that vanished without a `done` record.
const NO_DONE: &str = "exited without a done record";

/// The budget of a wait call: `timeout_secs` when given, clamped to
/// [`WAIT_BUDGET_MAX_SECS`]; otherwise `default`, which is the session's
/// bash ceiling for `wait_for` and [`WAIT_JOB_DEFAULT_SECS`] for
/// `wait_job`. A wait is never clamped by the ceiling itself.
pub(super) fn wait_budget(timeout_secs: Option<u64>, default: u64) -> Duration {
    Duration::from_secs(timeout_secs.map_or(default, |secs| secs.min(WAIT_BUDGET_MAX_SECS)))
}

/// The job log file: created empty for this process, removed when the
/// tool goes away. Commands append to it through `$GG_JOB_LOG`; nothing
/// here writes records.
pub(super) struct JobLog {
    path: PathBuf,
}

impl JobLog {
    /// Create the run's log under the temp directory, named with the
    /// process id, empty and readable only by its owner. An instance
    /// counter keeps the tools of one process apart, so a new session's
    /// tool never rewrites the one a live agent still hands out. A log
    /// that cannot be created leaves the path in place: commands append
    /// with `>>`, which creates the file, so the wait side still works.
    pub(super) fn create() -> Self {
        static INSTANCES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let instance = INSTANCES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("gg-jobs-{}-{instance}.log", std::process::id()));
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            let _ = file.flush();
        }
        Self { path }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for JobLog {
    /// Best effort: the log is a temp file, and a run that skips
    /// destructors leaves it to be swept with the rest of the directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What a registered job is in, latest record winning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JobState {
    /// No record for the name.
    Unknown,
    /// A `start` is the latest record: pid and the directory holding the
    /// job's three files.
    Running { pid: u32, dir: PathBuf },
    /// A `done` is the latest record. `exit` is `None` when a pid
    /// vanished without a `done`; `dir` is the `start` record's, and may
    /// be missing when a `done` arrived with no `start` before it.
    Finished {
        exit: Option<i32>,
        note: String,
        dir: Option<PathBuf>,
    },
}

/// One parsed record line, tied to its job name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Record {
    Start { pid: u32, dir: PathBuf },
    Done { exit: i32, note: String },
}

/// The state of every named job in the log, in name order. A missing log
/// reads as no records at all.
pub(super) fn read_all(log: &JobLog) -> BTreeMap<String, JobState> {
    let text = std::fs::read_to_string(log.path()).unwrap_or_default();
    let mut jobs: BTreeMap<String, JobState> = BTreeMap::new();
    for (name, record) in text.lines().filter_map(parse_line) {
        let state = match record {
            Record::Start { pid, dir } => JobState::Running { pid, dir },
            Record::Done { exit, note } => match jobs.get(&name) {
                // `done` after `start`: finished, with the start's
                // directory. A `done` with no `start` has none to name.
                Some(JobState::Running { dir, .. }) => JobState::Finished {
                    exit: Some(exit),
                    note,
                    dir: Some(dir.clone()),
                },
                _ => JobState::Finished {
                    exit: Some(exit),
                    note,
                    dir: None,
                },
            },
        };
        jobs.insert(name, state);
    }
    jobs
}

/// [`read_all`] for one name.
fn read_one(log: &JobLog, name: &str) -> JobState {
    read_all(log).remove(name).unwrap_or(JobState::Unknown)
}

/// One line: `start NAME PID DIR` or `done NAME EXIT NOTE`, tab
/// separated. Anything else is skipped.
fn parse_line(line: &str) -> Option<(String, Record)> {
    let mut fields = line.split('\t');
    let kind = fields.next()?;
    let name = fields.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let record = match kind {
        "start" => {
            let pid = fields.next()?.parse().ok()?;
            let dir = fields.next()?;
            if fields.next().is_some() || dir.is_empty() {
                return None;
            }
            Record::Start {
                pid,
                dir: PathBuf::from(dir),
            }
        }
        "done" => {
            let exit = fields.next()?.parse().ok()?;
            // The note is the last field and may be empty, written or
            // not; a fifth field breaks the contract and skips the line.
            let note = match fields.next() {
                Some(note) if fields.next().is_none() => note.to_string(),
                None => String::new(),
                _ => return None,
            };
            Record::Done { exit, note }
        }
        _ => return None,
    };
    Some((name, record))
}

/// Whether `pid` is still running, by `kill -0`.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The last `ERR_TAIL` bytes of `NAME.err`, for a failed or
/// still-running answer. A missing or unreadable file is no output.
fn err_tail(dir: Option<&Path>, name: &str) -> String {
    let Some(dir) = dir else {
        return String::new();
    };
    match std::fs::read(dir.join(format!("{name}.err"))) {
        Ok(bytes) => suffix_string(&bytes, ERR_TAIL),
        Err(_) => String::new(),
    }
}

/// What a finished job reports: the exit and its note, then where the
/// answer, the progress and the session live. A non-zero or unknown exit
/// is an error result, with the last of `NAME.err` as partial output.
fn finished_result(name: &str, state: JobState) -> ToolResult {
    let JobState::Finished { exit, note, dir } = state else {
        return ToolResult::err(format!("job {name:?} is not registered as finished"));
    };
    let exit = exit.map_or_else(|| "unknown".to_string(), |code| code.to_string());
    let mut message = format!("job {name} finished: exit {exit}");
    if !note.is_empty() {
        message.push_str(". ");
        message.push_str(&note);
    }
    if let Some(dir) = &dir {
        let out = dir.join(format!("{name}.out"));
        let bytes = std::fs::metadata(&out).map_or(0, |meta| meta.len());
        message.push_str(&format!(
            "\nanswer: {} ({bytes} bytes) · progress: {} · session: {}",
            out.display(),
            dir.join(format!("{name}.err")).display(),
            dir.join(format!("{name}.jsonl")).display(),
        ));
    }
    if exit == "0" {
        ToolResult::ok(message)
    } else {
        ToolResult::err(with_partial(message, err_tail(dir.as_deref(), name)))
    }
}

/// What a call says about a name with no record in the log.
fn unknown_job(log: &JobLog, name: &str) -> String {
    let registered: Vec<String> = read_all(log).keys().cloned().collect();
    let seen = if registered.is_empty() {
        "no job is registered in the job log yet".to_string()
    } else {
        format!("jobs registered in the job log: {}", registered.join(", "))
    };
    format!(
        "no job named {name:?}: {seen}. A job registers through $GG_JOB_LOG, \
         the log path every bash command receives: append a start record \
         (name, pid, directory) when it starts and a done record (name, \
         exit, note) when it ends"
    )
}

/// `wait_job: name`: wait for the detached job a command registered, up
/// to `budget`, until a `done` record lands or the pid is gone. A stop
/// request ends the wait, not the job.
pub(super) async fn wait(
    log: &JobLog,
    name: &str,
    budget: Duration,
    cancel: &Cancel,
) -> ToolResult {
    let started = Instant::now();
    let mut state = read_one(log, name);
    if state == JobState::Unknown {
        return ToolResult::err(unknown_job(log, name));
    }
    loop {
        match &state {
            JobState::Finished { .. } => return finished_result(name, state),
            JobState::Running { pid, dir } => {
                if !pid_alive(*pid) {
                    return finished_result(
                        name,
                        JobState::Finished {
                            exit: None,
                            note: NO_DONE.to_string(),
                            dir: Some(dir.clone()),
                        },
                    );
                }
            }
            JobState::Unknown => unreachable!("checked before the loop"),
        }
        if started.elapsed() >= budget {
            let JobState::Running { pid, dir } = &state else {
                unreachable!("running was matched above");
            };
            return ToolResult::ok(with_partial(
                format!(
                    "job {name} is still running after {}s (pid {pid}) · {}; \
                     call bash with wait_job: {name} to keep waiting",
                    started.elapsed().as_secs(),
                    dir.display(),
                ),
                err_tail(Some(dir), name),
            ));
        }
        tokio::select! {
            biased;
            () = stop_requested(cancel) => {
                return ToolResult::err(format!(
                    "wait for job {name} aborted by user; the job keeps running"
                ));
            }
            () = tokio::time::sleep(POLL) => {}
        }
        state = read_one(log, name);
    }
}
