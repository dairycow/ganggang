//! Warning text for failed hooks. Kept apart from the process plumbing so
//! the message shape is testable and the runner stays small.

use super::{Hook, HookEvent, tail_text};

const WARNING_TAIL_CAP: usize = 1024;

pub(super) fn failed(event: HookEvent, hook: &Hook, status: &str, stderr: &str) -> String {
    format!(
        "{} hook `{}` {status}{}",
        event.as_str(),
        hook.command,
        warning_tail(stderr)
    )
}

pub(super) fn failed_message(event: HookEvent, hook: &Hook, message: &str) -> String {
    format!(
        "{} hook `{}` failed: {message}",
        event.as_str(),
        hook.command
    )
}

pub(super) fn timed_out(event: HookEvent, hook: &Hook) -> String {
    format!(
        "{} hook `{}` timed out after {}s",
        event.as_str(),
        hook.command,
        hook.timeout_secs
    )
}

fn warning_tail(stderr: &str) -> String {
    let flat = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    format!(": {}", tail_text(&flat, WARNING_TAIL_CAP))
}

/// A `pre_tool` hook that printed something other than a valid decision is
/// a failure; its stderr rides along like any other failure.
pub(super) fn invalid_output(event: HookEvent, hook: &Hook, error: &str, stderr: &str) -> String {
    format!(
        "{} hook `{}` printed invalid output: {error}{}",
        event.as_str(),
        hook.command,
        warning_tail(stderr)
    )
}
