//! `!`/`!!` shell execution for the TUI: the same bounded runner the bash
//! tool uses (one spawn path, one kill discipline — an orphaned process
//! group is an orphaned process group whoever asked for the command).

use std::path::PathBuf;

use tokio::sync::mpsc;

use super::agent_task::UiEvent;

/// `!cmd` shell budget: output cap and timeout, matching the bash tool's
/// caps.
pub(super) const SHELL_OUTPUT_CAP: usize = 10 * 1024;
pub(super) const SHELL_TIMEOUT_SECS: u64 = 30;

/// Run one `!`/`!!` shell line and report it back to the UI.
pub(super) fn spawn_shell(
    command: String,
    cwd: PathBuf,
    to_context: bool,
    ui: mpsc::Sender<UiEvent>,
) {
    tokio::spawn(async move {
        let (output, exit_code) = run_shell(&command, &cwd).await;
        let _ = ui
            .send(UiEvent::ShellDone {
                command,
                output,
                exit_code,
                to_context,
            })
            .await;
    });
}

pub(super) async fn run_shell(command: &str, cwd: &std::path::Path) -> (String, Option<i32>) {
    use crate::agent::tools::Cancel;
    use crate::agent::tools::bash::BashOutcome;

    // A flag nothing ever sets: a `!` line is typed by the user at their own
    // keyboard, it is not part of a run, and Esc belongs to the run — reusing
    // the run's abort flag here would also inherit its latch (the loop
    // clears it at run entry, the shell path does not), so a stale Esc would
    // silently kill the next `!` command. Bounded by the timeout instead.
    let cancel = Cancel::none();
    let timeout = std::time::Duration::from_secs(SHELL_TIMEOUT_SECS);
    match crate::agent::tools::bash::run_bash(command, cwd, timeout, SHELL_OUTPUT_CAP, &cancel)
        .await
    {
        BashOutcome::Done { output, status } => (output, status.code()),
        BashOutcome::Cancelled => ("command aborted by user and killed".to_string(), None),
        BashOutcome::TimedOut => (
            format!("command timed out after {SHELL_TIMEOUT_SECS}s and was killed"),
            None,
        ),
        BashOutcome::Failed(message) => (message, None),
    }
}
