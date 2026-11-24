//! `bash`: run a shell command, capture everything, report the exit code.
//!
//! Runs with the user's permissions in the session cwd — the documented
//! trust boundary (spec §10). Everything that can hang has a number: the
//! per-call timeout is clamped to the configured maximum. And everything
//! that can hang can also be stopped: the user's abort takes down the
//! whole process group, the same kill the timeout uses.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, parse_args};

const OUTPUT_CAP: usize = 10 * 1024;

pub struct BashTool {
    cwd: PathBuf,
    max_timeout_secs: u64,
}

impl BashTool {
    pub fn new(cwd: PathBuf, max_timeout_secs: u64) -> Self {
        Self {
            cwd,
            max_timeout_secs,
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

/// Why [`settle`] stopped a command that did not finish on its own.
enum Stop {
    TimedOut,
    Cancelled,
    Failed(String),
}

/// The bounded head and tail of a command's merged output. The pipe is
/// still drained to EOF; only these two windows are retained, so an
/// endless stream costs O(cap) memory.
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl Capture {
    fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, bytes: &[u8], head_cap: usize, tail_cap: usize) {
        self.total += bytes.len();
        let head_room = head_cap.saturating_sub(self.head.len());
        let take = head_room.min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        for &b in &bytes[take..] {
            self.tail.push_back(b);
            if self.tail.len() > tail_cap {
                self.tail.pop_front();
            }
        }
    }
}

/// The longest valid UTF-8 prefix of `bytes`, trimming only an
/// incomplete character split at the end.
fn prefix_string(bytes: &[u8]) -> String {
    let mut bytes = bytes;
    if !bytes.is_empty() {
        let mut start = bytes.len() - 1;
        let floor = start.saturating_sub(3);
        while start > floor && (bytes[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        if let Err(e) = std::str::from_utf8(&bytes[start..])
            && e.valid_up_to() == 0
            && e.error_len().is_none()
        {
            bytes = &bytes[..start];
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The longest valid UTF-8 suffix of `bytes`, dropping a leading partial
/// character left by the split.
fn suffix_string(bytes: &[u8]) -> String {
    let mut skip = 0;
    while skip < bytes.len().min(3) && (bytes[skip] & 0xC0) == 0x80 {
        skip += 1;
    }
    String::from_utf8_lossy(&bytes[skip..]).into_owned()
}

/// Render the captured bytes: everything when it fits, otherwise the head,
/// a short marker, and the tail. Both splits are trimmed to char boundaries,
/// so the result stays below `cap + 100` bytes.
fn render_capture(capture: &Arc<Mutex<Capture>>, cap: usize) -> String {
    let Ok(mut capture) = capture.lock() else {
        return String::new();
    };
    if capture.total <= cap {
        let mut all = capture.head.clone();
        all.extend(capture.tail.iter().copied());
        return String::from_utf8_lossy(&all).into_owned();
    }
    let omitted = capture.total.saturating_sub(cap);
    let head = prefix_string(&capture.head);
    let tail = suffix_string(capture.tail.make_contiguous());
    format!("{head}\n[... {omitted} bytes omitted ...]\n{tail}")
}

/// The pause that notices a stop request while a command runs: the same
/// cadence the hooks use, cheap for the waiting task and quick for the user.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Group-wide SIGKILL via /bin/kill — no `unsafe` in our own code (spec
/// §01). The group may already be gone; ignore.
async fn kill_group(pgid: Option<u32>) {
    if let Some(pgid) = pgid {
        let _ = tokio::process::Command::new("kill")
            .arg("-9")
            .arg(format!("-{pgid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// Resolve the run against two deadlines: the command's timeout and the
/// user's stop request. Whichever fires first takes down the whole process
/// group; the winner is reported so the caller can tell the user which of
/// the two happened.
///
/// `Ok(status)` means the command finished on its own. On a kill the wait
/// future is dropped, which is what the spawn's `kill_on_drop` is for: the
/// shell is reaped even if the group signal misses it.
async fn settle(
    wait: impl std::future::Future<Output = std::io::Result<((), std::process::ExitStatus)>>,
    timeout: Duration,
    cancel: &Cancel,
    pgid: Option<u32>,
) -> Result<std::process::ExitStatus, Stop> {
    tokio::pin!(wait);
    // Cancellation is polled, not awaited: an `AtomicBool` has no event
    // source, and a parked `sleep 30` must die within one poll.
    let polled = async {
        loop {
            if cancel.is_cancelled() {
                return;
            }
            tokio::time::sleep(CANCEL_POLL).await;
        }
    };
    tokio::pin!(polled);
    tokio::select! {
        biased;
        // A command that finished wins outright, polled first exactly as
        // `tokio::time::timeout` polled it — output is not lost to a stop
        // request that arrived as it was ending.
        result = &mut wait => match result {
            Ok(((), status)) => Ok(status),
            Err(e) => Err(Stop::Failed(format!("waited on bash: {e}"))),
        },
        // While it is still running, the stop request outranks the
        // deadline: reporting a timeout for what the user asked for would
        // hide what their Esc actually did.
        () = &mut polled => {
            kill_group(pgid).await;
            Err(Stop::Cancelled)
        }
        () = tokio::time::sleep(timeout) => {
            kill_group(pgid).await;
            Err(Stop::TimedOut)
        }
    }
}

/// Run `bash -c command` in `cwd`: streams merged inside the shell, head
/// and tail kept within `cap` bytes and the rest drained (bounded memory,
/// real exit code), process-group kill on timeout **and** on cancellation.
pub(crate) async fn run_bash(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cap: usize,
    cancel: &Cancel,
) -> BashOutcome {
    // `exec 2>&1` merges the streams inside the shell, so ordering is the
    // real arrival order and no pipe plumbing is needed to interleave.
    let script = format!("exec 2>&1\n{command}");
    let mut child = match tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Own process group: every kill — timeout or the user's stop —
        // must take down the whole tree; killing only the shell leaves
        // pipelines and background children running. Group id == child pid
        // (it is the leader).
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return BashOutcome::Failed(format!("failed to spawn bash: {e}")),
    };
    let pgid = child.id();

    // Drain the pipe to EOF, retaining only a bounded head and tail. The
    // capture is shared so a kill can still report what arrived first.
    let mut stdout: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match child.stdout.take() {
        Some(s) => Box::new(s),
        None => Box::new(tokio::io::empty()),
    };
    let head_cap = cap / 4;
    let tail_cap = cap - head_cap;
    let capture = Arc::new(Mutex::new(Capture::new()));
    let reader_capture = Arc::clone(&capture);
    let mut sink = [0u8; 8192];
    let read_to_end = async move {
        use tokio::io::AsyncReadExt;
        loop {
            let n = stdout.read(&mut sink).await?;
            if n == 0 {
                return Ok(());
            }
            if let Ok(mut capture) = reader_capture.lock() {
                capture.push(&sink[..n], head_cap, tail_cap);
            }
            // A stream that never EOFs (`cat /dev/zero`) is always
            // read-ready: without this the loop never returns Pending
            // and starves the timeout/cancel branches of `settle`.
            tokio::task::yield_now().await;
        }
    };
    let status = match settle(
        // Reading to the end and waiting are one unit: waiting without the
        // read deadlocks once the pipe fills.
        async { tokio::try_join!(read_to_end, child.wait()) },
        timeout,
        cancel,
        pgid,
    )
    .await
    {
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
 include the output; a timeout or abort reports any partial output."
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
            match run_bash(&args.command, &self.cwd, timeout, OUTPUT_CAP, cancel).await {
                BashOutcome::Done { output, status } => {
                    let code = match status.code() {
                        Some(c) => c.to_string(),
                        None => "killed by signal".to_string(),
                    };
                    let result = format!("[exit code: {code}]\n{output}");
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
