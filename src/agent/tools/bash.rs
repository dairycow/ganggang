//! `bash`: run a shell command, capture everything, report the exit code.
//!
//! Runs with the user's permissions in the session cwd — the documented
//! trust boundary (spec §10). Everything that can hang has a number: the
//! per-call timeout is clamped to the configured maximum. And everything
//! that can hang can also be stopped: the user's abort takes down the
//! whole process group, the same kill the timeout uses.

use std::path::{Path, PathBuf};
use std::process::Stdio;
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
    /// Ran to completion; `output` is capped at `cap` bytes with a
    /// visible truncation notice.
    Done {
        output: String,
        status: std::process::ExitStatus,
    },
    /// Killed at the deadline, whole process group included.
    TimedOut,
    /// Killed because the user stopped the run, whole process group
    /// included.
    Cancelled,
    /// Never started or unwaitable; the string is a user-facing message.
    Failed(String),
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
) -> Result<std::process::ExitStatus, BashOutcome> {
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
            Err(e) => Err(BashOutcome::Failed(format!("waited on bash: {e}"))),
        },
        // While it is still running, the stop request outranks the
        // deadline: reporting a timeout for what the user asked for would
        // hide what their Esc actually did.
        () = &mut polled => {
            kill_group(pgid).await;
            Err(BashOutcome::Cancelled)
        }
        () = tokio::time::sleep(timeout) => {
            kill_group(pgid).await;
            Err(BashOutcome::TimedOut)
        }
    }
}

/// Run `bash -c command` in `cwd`: streams merged inside the shell, output
/// stored up to `cap` bytes and drained past it (bounded memory, real exit
/// code), process-group kill on timeout **and** on cancellation.
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

    // Store at most cap+1 bytes (the +1 proves truncation) and discard the
    // rest: buffering everything (wait_with_output) would let `cat
    // /dev/zero` grow memory without bound. Draining (instead of closing
    // the pipe) lets the command run to completion for its exit code.
    let mut stdout: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match child.stdout.take() {
        Some(s) => Box::new(s),
        None => Box::new(tokio::io::empty()),
    };
    let mut stored: Vec<u8> = Vec::new();
    let mut sink = [0u8; 8192];
    let read_to_end = async {
        use tokio::io::AsyncReadExt;
        loop {
            let n = stdout.read(&mut sink).await?;
            if n == 0 {
                return Ok(());
            }
            if stored.len() < cap + 1 {
                let room = cap + 1 - stored.len();
                stored.extend_from_slice(&sink[..n.min(room)]);
            }
        }
    };
    let status = match settle(
        // Reading to the end and waiting are one unit: an uncapped read
        // would let the command outrun the store, and waiting without the
        // read deadlocks once the pipe fills.
        async { tokio::try_join!(read_to_end, child.wait()) },
        timeout,
        cancel,
        pgid,
    )
    .await
    {
        Ok(status) => status,
        Err(outcome) => return outcome,
    };

    let mut text = String::from_utf8_lossy(&stored).to_string();
    if text.len() > cap {
        // Cut at the last char boundary within the cap — a byte-index
        // truncate would panic on multibyte output.
        let cut = text
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= cap)
            .last()
            .unwrap_or(0);
        text.truncate(cut);
        text.push_str(&format!("\n[truncated at {cap} bytes]"));
    }
    BashOutcome::Done {
        output: text,
        status,
    }
}

impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Run a command in bash and return the merged stdout+stderr and exit code. \
Nonzero exits are reported as errors with the output included."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "The shell command to run"},
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
                BashOutcome::TimedOut => ToolResult::err(format!(
                    "command timed out after {}s and was killed",
                    timeout.as_secs()
                )),
                // Distinct from a timeout: the user stopped this one, and
                // the wording must not read like the command was too slow.
                BashOutcome::Cancelled => {
                    ToolResult::err("command aborted by user and killed".into())
                }
                BashOutcome::Failed(message) => ToolResult::err(message),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A cancel flag nothing sets — the tests here are about running and
    /// timing out, not about the user stopping a command.
    fn no_cancel() -> Cancel {
        Cancel::none()
    }

    async fn run(cmd: &str, timeout: Option<u64>) -> ToolResult {
        let tool = BashTool::new(std::env::temp_dir(), 30);
        let args = match timeout {
            Some(t) => serde_json::json!({"command": cmd, "timeout_secs": t}),
            None => serde_json::json!({"command": cmd}),
        };
        tool.execute(&args, &no_cancel()).await
    }

    #[tokio::test]
    async fn runs_a_command_and_reports_the_exit_code() {
        let r = run("echo hi", None).await;
        assert!(!r.is_error);
        assert!(r.output.contains("[exit code: 0]"));
        assert!(r.output.contains("hi"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error_result_with_output() {
        let r = run("echo boom >&2; false", None).await;
        assert!(r.is_error);
        assert!(r.output.contains("[exit code: 1]"));
        assert!(
            r.output.contains("boom"),
            "stderr must be captured: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_command_and_clamps_to_the_max() {
        let tool = BashTool::new(std::env::temp_dir(), 1);
        let started = std::time::Instant::now();
        let r = tool
            .execute(
                &serde_json::json!({"command": "sleep 30", "timeout_secs": 999}),
                &no_cancel(),
            )
            .await;
        assert!(r.is_error);
        assert!(r.output.contains("timed out after 1s"));
        assert!(started.elapsed().as_secs() < 5, "kill must be prompt");
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_process_group_not_just_the_shell() {
        // A pipeline means bash forks children; killing only bash would
        // leave them running past the deadline. The marker doubles as the
        // (unique, valid) sleep duration so pgrep can find group members.
        let marker = "617.319";
        let tool = BashTool::new(std::env::temp_dir(), 1);
        let r = tool
            .execute(
                &serde_json::json!({
                    "command": format!("sleep {marker} | sleep {marker} & sleep {marker}")
                }),
                &no_cancel(),
            )
            .await;
        assert!(r.is_error);
        assert!(r.output.contains("timed out after 1s"));

        // The group kill is asynchronous; give it a moment, then require
        // that no member of the group survived.
        for _ in 0..20 {
            let alive = std::process::Command::new("pgrep")
                .arg("-f")
                .arg(marker)
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        panic!("process-group members survived the timeout kill");
    }

    /// Esc while a command is running: a stop request raised mid-flight
    /// ends the call inside one poll of the pause, and the whole process
    /// group goes down with the shell — the timeout's kill discipline, for
    /// a different reason. The marker doubles as the (unique, valid) sleep
    /// duration so pgrep can find group members.
    #[tokio::test]
    async fn cancellation_kills_the_whole_process_group_and_reports_it() {
        let marker = "731.207";
        let tool = BashTool::new(std::env::temp_dir(), 30);
        let flag = Arc::new(AtomicBool::new(false));
        let setter = {
            let flag = flag.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                flag.store(true, Ordering::Relaxed);
            })
        };

        let started = std::time::Instant::now();
        let r = tool
            .execute(
                // A pipeline: bash forks children, so a shell-only kill
                // would leave sleepers running past the stop request.
                &serde_json::json!({"command": format!("sleep {marker} | sleep {marker}")}),
                &Cancel::new(flag),
            )
            .await;
        setter.await.unwrap();

        assert!(r.is_error);
        assert!(
            r.output.contains("aborted by user and killed"),
            "{}",
            r.output
        );
        assert!(
            !r.output.contains("timed out"),
            "the user stopped this one, so it must not read like a slow command: {}",
            r.output
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel must be prompt"
        );

        // The group kill is asynchronous; give it a moment, then require
        // that no member of the group survived.
        for _ in 0..20 {
            let alive = std::process::Command::new("pgrep")
                .arg("-f")
                .arg(marker)
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("process-group members survived the cancellation kill");
    }

    /// A long timeout must not outlive the stop request: the answer names
    /// cancellation, never the deadline that never fired.
    #[tokio::test]
    async fn cancellation_beats_a_timeout_that_was_never_reached() {
        let tool = BashTool::new(std::env::temp_dir(), 300);
        let flag = Arc::new(AtomicBool::new(false));
        let setter = {
            let flag = flag.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                flag.store(true, Ordering::Relaxed);
            })
        };

        let started = std::time::Instant::now();
        let r = tool
            .execute(
                &serde_json::json!({"command": "sleep 200", "timeout_secs": 200}),
                &Cancel::new(flag),
            )
            .await;
        setter.await.unwrap();

        assert_eq!(r.output, "command aborted by user and killed");
        assert!(r.is_error);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "no waiting on the deadline"
        );
    }

    #[tokio::test]
    async fn output_is_truncated_at_the_cap() {
        let r = run("printf 'a%.0s' {1..12000}", None).await;
        assert!(!r.is_error);
        assert!(r.output.contains("[truncated at 10240 bytes]"));
        assert!(r.output.len() < OUTPUT_CAP + 200);
    }

    #[tokio::test]
    async fn an_endless_stream_cannot_grow_memory_without_bound() {
        // /dev/zero never EOFs: the capped read loop stores 10 KiB and
        // discards the rest until the timeout kills the group.
        let tool = BashTool::new(std::env::temp_dir(), 1);
        let started = std::time::Instant::now();
        let r = tool
            .execute(
                &serde_json::json!({"command": "cat /dev/zero"}),
                &no_cancel(),
            )
            .await;
        assert!(r.is_error);
        assert!(r.output.contains("timed out after 1s"));
        assert!(started.elapsed().as_secs() < 5);
    }
}
