//! Resolving a running command against its two deadlines: the command's
//! timeout and the user's stop request. Whichever fires first takes down
//! the whole process group, the same kill for both, so no command outlives
//! the run that asked for it.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::ParentEnv;
use super::output::{Capture, SharedCapture};
use crate::agent::tools::Cancel;

/// Why [`settle`] stopped a command that did not finish on its own.
pub(super) enum Stop {
    TimedOut,
    Cancelled,
    Failed(String),
}

/// The pause that notices a stop request while a command runs: the same
/// cadence the hooks use, cheap for the waiting task and quick for the user.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Group-wide SIGKILL via /bin/kill — no `unsafe` in our own code (spec
/// §01). The group may already be gone; ignore.
pub(super) async fn kill_group(pgid: Option<u32>) {
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

/// Resolve once the user's stop request is in effect. Cancellation is
/// polled, not awaited: an `AtomicBool` has no event source, and a parked
/// `sleep 30` must die within one poll.
pub(super) async fn stop_requested(cancel: &Cancel) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// A command that has been started: its process group, what it has printed
/// so far and the child that owns both. A foreground run and a background
/// job both start from one of these.
pub(super) struct Spawned {
    pub(super) child: tokio::process::Child,
    pub(super) pgid: Option<u32>,
    pub(super) capture: SharedCapture,
}

/// Start `bash -c command` in `cwd` as the leader of its own process
/// group, with its output piped. `nest` is the run's, for the nested runs
/// the command may start; `None` leaves the command's environment alone.
pub(super) fn spawn(
    command: &str,
    cwd: &Path,
    nest: Option<&ParentEnv>,
) -> std::io::Result<Spawned> {
    // `exec 2>&1` merges the streams inside the shell, so ordering is the
    // real arrival order and no pipe plumbing is needed to interleave.
    let script = format!("exec 2>&1\n{command}");
    let mut spawning = tokio::process::Command::new("bash");
    spawning
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
        .kill_on_drop(true);
    if let Some(nest) = nest {
        super::apply_env(&mut spawning, nest.exports());
    }
    let child = spawning.spawn()?;
    let pgid = child.id();
    Ok(Spawned {
        child,
        pgid,
        capture: Arc::new(Mutex::new(Capture::new())),
    })
}

/// Drain the command's pipe to EOF, retaining only a bounded head and tail
/// (bounded memory), then wait for its exit status. The capture is shared,
/// so a kill can still report what arrived first. `on_exit` runs once the
/// shell has been reaped, which may be long before the pipe closes.
///
/// Reading to the end and waiting are one unit: waiting without the read
/// deadlocks once the pipe fills.
pub(super) async fn drain_and_wait(
    mut spawned: Spawned,
    cap: usize,
    on_exit: impl FnOnce(),
) -> std::io::Result<std::process::ExitStatus> {
    let mut stdout: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match spawned.child.stdout.take()
    {
        Some(s) => Box::new(s),
        None => Box::new(tokio::io::empty()),
    };
    let head_cap = cap / 4;
    let tail_cap = cap - head_cap;
    let reader_capture = Arc::clone(&spawned.capture);
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
    let wait = async move {
        let status = spawned.child.wait().await;
        on_exit();
        status
    };
    let ((), status) = tokio::try_join!(read_to_end, wait)?;
    Ok(status)
}

/// Resolve the run against two deadlines: the command's timeout and the
/// user's stop request. Whichever fires first takes down the whole process
/// group; the winner is reported so the caller can tell the user which of
/// the two happened.
///
/// `Ok(status)` means the command finished on its own. On a kill the wait
/// future is dropped, which is what the spawn's `kill_on_drop` is for: the
/// shell is reaped even if the group signal misses it.
pub(super) async fn settle(
    wait: impl std::future::Future<Output = std::io::Result<std::process::ExitStatus>>,
    timeout: Duration,
    cancel: &Cancel,
    pgid: Option<u32>,
) -> Result<std::process::ExitStatus, Stop> {
    tokio::pin!(wait);
    tokio::select! {
        biased;
        // A command that finished wins outright, polled first exactly as
        // `tokio::time::timeout` polled it — output is not lost to a stop
        // request that arrived as it was ending.
        result = &mut wait => match result {
            Ok(status) => Ok(status),
            Err(e) => Err(Stop::Failed(format!("waited on bash: {e}"))),
        },
        // While it is still running, the stop request outranks the
        // deadline: reporting a timeout for what the user asked for would
        // hide what their Esc actually did.
        () = stop_requested(cancel) => {
            kill_group(pgid).await;
            Err(Stop::Cancelled)
        }
        () = tokio::time::sleep(timeout) => {
            kill_group(pgid).await;
            Err(Stop::TimedOut)
        }
    }
}
