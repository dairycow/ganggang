//! The one-shot hook process: JSON on stdin, bounded streams, and the
//! same timeout/abort process-group discipline as the bash tool.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::payload::Payload;
use super::{Hook, cap_text, tail_text};

const STDOUT_CAP: usize = 64 * 1024;
const STDERR_CAP: usize = 8 * 1024;
const ABORT_POLL: Duration = Duration::from_millis(100);

/// Kills the hook's process group if the future is dropped before a normal
/// wait. External cancellation (TUI quit, a second SIGINT) otherwise only
/// reaches `kill_on_drop`, which takes the leader and leaves grandchildren.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    fn new(pgid: Option<u32>) -> Self {
        Self(pgid)
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.0 {
            // Synchronous and reaped: a spawned `kill` dropped un-waited
            // would leave a zombie on every cancellation.
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(format!("-{pgid}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

pub(super) enum RunOutcome {
    Done {
        stdout: String,
        stderr: String,
        success: bool,
        status: String,
    },
    Failed {
        message: String,
    },
    TimedOut,
    Interrupted,
}

pub(super) async fn run_hook(
    hook: &Hook,
    payload: &Payload,
    cwd: &Path,
    abort: &AtomicBool,
) -> RunOutcome {
    let payload = match serde_json::to_vec(payload) {
        Ok(bytes) => bytes,
        Err(e) => {
            return RunOutcome::Failed {
                message: format!("serializing payload: {e}"),
            };
        }
    };
    let mut child = match tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&hook.command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Same discipline as the bash tool: the timeout and abort kill the
        // whole tree, not just the shell (spec §10, §22).
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return RunOutcome::Failed {
                message: format!("failed to spawn bash: {e}"),
            };
        }
    };
    let pgid = child.id();
    let mut guard = GroupGuard::new(pgid);
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let work = async move {
        let write = async move {
            let Some(mut stdin) = stdin else {
                return Ok(());
            };
            use tokio::io::AsyncWriteExt;
            match stdin.write_all(&payload).await {
                Ok(()) => {
                    let _ = stdin.shutdown().await;
                }
                // A hook that ignores stdin is fine; EPIPE is not failure.
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => return Err(format!("writing hook stdin: {e}")),
            }
            Ok(())
        };
        let read_out = read_capped(stdout, STDOUT_CAP);
        let read_err = read_tail(stderr, STDERR_CAP);
        let wait = async move {
            child
                .wait()
                .await
                .map_err(|e| format!("waiting on hook: {e}"))
        };
        let ((), stdout, stderr, status) = tokio::try_join!(write, read_out, read_err, wait)?;
        Ok::<_, String>((stdout, stderr, status))
    };

    let outcome = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(hook.timeout_secs), work) => result,
        () = abort_when(abort) => {
            kill_group(pgid).await;
            guard.disarm();
            return RunOutcome::Interrupted;
        }
    };
    match outcome {
        Err(_) => {
            kill_group(pgid).await;
            guard.disarm();
            RunOutcome::TimedOut
        }
        Ok(Err(message)) => {
            // A pipe/read error can leave the shell running; a failed hook
            // must not leak a process tree (§22).
            kill_group(pgid).await;
            guard.disarm();
            RunOutcome::Failed { message }
        }
        Ok(Ok((stdout, stderr, status))) => {
            // The child is reaped; the pgid may be recycled immediately.
            guard.disarm();
            RunOutcome::Done {
                stdout,
                stderr,
                success: status.success(),
                status: status.code().map_or_else(
                    || "killed by signal".to_string(),
                    |code| format!("exit {code}"),
                ),
            }
        }
    }
}

async fn abort_when(abort: &AtomicBool) {
    while !abort.load(Ordering::Relaxed) {
        tokio::time::sleep(ABORT_POLL).await;
    }
}

async fn kill_group(pgid: Option<u32>) {
    if let Some(pgid) = pgid {
        // Group-wide SIGKILL via /bin/kill — no `unsafe` in our code
        // (spec §01), and the group may already be gone.
        let _ = tokio::process::Command::new("kill")
            .arg("-9")
            .arg(format!("-{pgid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

async fn read_capped<R>(reader: Option<R>, cap: usize) -> Result<String, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut reader) = reader else {
        return Ok(String::new());
    };
    let mut stored = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| format!("reading hook output: {e}"))?;
        if n == 0 {
            break;
        }
        if stored.len() < cap + 1 {
            let room = cap + 1 - stored.len();
            stored.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
    Ok(cap_text(&String::from_utf8_lossy(&stored), cap))
}

/// Like [`read_capped`], but keeps the *end* of the stream — stderr's last
/// bytes are the error, not its first banner (§22).
async fn read_tail<R>(reader: Option<R>, cap: usize) -> Result<String, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut reader) = reader else {
        return Ok(String::new());
    };
    let mut stored: VecDeque<u8> = VecDeque::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| format!("reading hook output: {e}"))?;
        if n == 0 {
            break;
        }
        let bytes = &chunk[..n];
        if bytes.len() > cap + 1 {
            stored.clear();
            stored.extend(&bytes[bytes.len() - (cap + 1)..]);
        } else {
            let overflow = (stored.len() + bytes.len()).saturating_sub(cap + 1);
            for _ in 0..overflow {
                stored.pop_front();
            }
            stored.extend(bytes);
        }
    }
    let bytes: Vec<u8> = stored.into_iter().collect();
    Ok(tail_text(&String::from_utf8_lossy(&bytes), cap))
}
