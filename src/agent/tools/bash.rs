//! `bash`: run a shell command, capture everything, report the exit code.
//!
//! Runs with the user's permissions in the session cwd — the documented
//! trust boundary (spec §10). Everything that can hang has a number: the
//! per-call timeout is clamped to the configured maximum.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Tool, ToolResult, parse_args};

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
    /// Never started or unwaitable; the string is a user-facing message.
    Failed(String),
}

/// Run `bash -c command` in `cwd`: streams merged inside the shell, output
/// stored up to `cap` bytes and drained past it (bounded memory, real exit
/// code), process-group kill on timeout.
pub(crate) async fn run_bash(
    command: &str,
    cwd: &Path,
    timeout: Duration,
    cap: usize,
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
        // Own process group: a timeout must take down the whole tree —
        // killing only the shell leaves pipelines and background children
        // running past the deadline. Group id == child pid (it is the leader).
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
    let status = tokio::time::timeout(timeout, async {
        tokio::try_join!(read_to_end, child.wait())
    })
    .await;
    let status = match status {
        Ok(Ok(((), status))) => status,
        Ok(Err(e)) => return BashOutcome::Failed(format!("waited on bash: {e}")),
        Err(_) => {
            if let Some(pgid) = pgid {
                // Group-wide SIGKILL via /bin/kill — no `unsafe` in our own
                // code (spec §01). The group may already be gone; ignore.
                let _ = tokio::process::Command::new("kill")
                    .arg("-9")
                    .arg(format!("-{pgid}"))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
            return BashOutcome::TimedOut;
        }
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

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
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
            match run_bash(&args.command, &self.cwd, timeout, OUTPUT_CAP).await {
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
                BashOutcome::Failed(message) => ToolResult::err(message),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(cmd: &str, timeout: Option<u64>) -> ToolResult {
        let tool = BashTool::new(std::env::temp_dir(), 30);
        let args = match timeout {
            Some(t) => serde_json::json!({"command": cmd, "timeout_secs": t}),
            None => serde_json::json!({"command": cmd}),
        };
        tool.execute(&args).await
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
            .execute(&serde_json::json!({"command": "sleep 30", "timeout_secs": 999}))
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
            .execute(&serde_json::json!({
                "command": format!("sleep {marker} | sleep {marker} & sleep {marker}")
            }))
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
            .execute(&serde_json::json!({"command": "cat /dev/zero"}))
            .await;
        assert!(r.is_error);
        assert!(r.output.contains("timed out after 1s"));
        assert!(started.elapsed().as_secs() < 5);
    }
}
