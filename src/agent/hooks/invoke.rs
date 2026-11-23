//! The hook decision points: pre/post tool and run end. Values in,
//! warnings/decisions out — only the loop emits events.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;

use super::payload::Payload;
use super::report::{failed, failed_message, invalid_output, timed_out};
use super::run::{RunOutcome, run_hook};
use super::{Hook, HookEvent, Hooks, PreOutcome, REASON_CAP, cap_text};
use crate::agent::session::ToolCall;
use crate::agent::tools::ToolResult;

impl Hooks {
    /// Run matching pre-tool hooks in file order. The first deny wins.
    pub async fn pre_tool(
        &self,
        call: &ToolCall,
        session_id: Option<&str>,
        cwd: &Path,
        abort: &AtomicBool,
    ) -> PreOutcome {
        let mut warnings = Vec::new();
        let matched: Vec<&Hook> = self.matching(HookEvent::PreTool, &call.name).collect();
        if matched.is_empty() {
            return PreOutcome::Allow { warnings };
        }
        let payload = Payload::new(HookEvent::PreTool, cwd, session_id).with_tool(call);
        for hook in matched {
            match run_hook(hook, &payload, cwd, abort).await {
                RunOutcome::Done {
                    stdout,
                    stderr,
                    success: true,
                    ..
                } => match parse_decision(&stdout) {
                    Ok(Decision::Allow) => {}
                    Ok(Decision::Deny(reason)) => {
                        return PreOutcome::Deny {
                            reason: deny_reason(reason, hook),
                            warnings,
                        };
                    }
                    Err(e) => warnings.push(invalid_output(HookEvent::PreTool, hook, &e, &stderr)),
                },
                RunOutcome::Done {
                    stderr,
                    success: false,
                    status,
                    ..
                } => warnings.push(failed(HookEvent::PreTool, hook, &status, &stderr)),
                RunOutcome::Failed { message } => {
                    warnings.push(failed_message(HookEvent::PreTool, hook, &message));
                }
                RunOutcome::TimedOut => warnings.push(timed_out(HookEvent::PreTool, hook)),
                RunOutcome::Interrupted => return PreOutcome::Interrupted,
            }
        }
        PreOutcome::Allow { warnings }
    }

    /// Run matching post-tool hooks. The result is already recorded; these
    /// can only warn.
    pub async fn post_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        session_id: Option<&str>,
        cwd: &Path,
        abort: &AtomicBool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let matched: Vec<&Hook> = self.matching(HookEvent::PostTool, &call.name).collect();
        if matched.is_empty() {
            return warnings;
        }
        let payload = Payload::new(HookEvent::PostTool, cwd, session_id)
            .with_tool(call)
            .with_result(result);
        for hook in matched {
            match run_hook(hook, &payload, cwd, abort).await {
                RunOutcome::Done { success: true, .. } => {}
                RunOutcome::Done {
                    stderr,
                    success: false,
                    status,
                    ..
                } => warnings.push(failed(HookEvent::PostTool, hook, &status, &stderr)),
                RunOutcome::Failed { message } => {
                    warnings.push(failed_message(HookEvent::PostTool, hook, &message));
                }
                RunOutcome::TimedOut => warnings.push(timed_out(HookEvent::PostTool, hook)),
                RunOutcome::Interrupted => break,
            }
        }
        warnings
    }

    /// Run run-end hooks once per clean run. Observe only.
    pub async fn run_end(
        &self,
        turns: u32,
        session_id: Option<&str>,
        cwd: &Path,
        abort: &AtomicBool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let matched: Vec<&Hook> = self
            .specs
            .iter()
            .filter(|h| h.event == HookEvent::RunEnd)
            .collect();
        if matched.is_empty() {
            return warnings;
        }
        let mut payload = Payload::new(HookEvent::RunEnd, cwd, session_id);
        payload.turns = Some(turns);
        for hook in matched {
            match run_hook(hook, &payload, cwd, abort).await {
                RunOutcome::Done { success: true, .. } => {}
                RunOutcome::Done {
                    stderr,
                    success: false,
                    status,
                    ..
                } => warnings.push(failed(HookEvent::RunEnd, hook, &status, &stderr)),
                RunOutcome::Failed { message } => {
                    warnings.push(failed_message(HookEvent::RunEnd, hook, &message));
                }
                RunOutcome::TimedOut => warnings.push(timed_out(HookEvent::RunEnd, hook)),
                RunOutcome::Interrupted => break,
            }
        }
        warnings
    }

    fn matching<'a>(&'a self, event: HookEvent, tool: &'a str) -> impl Iterator<Item = &'a Hook> {
        self.specs
            .iter()
            .filter(move |h| h.event == event && h.matches(tool))
    }
}

enum Decision {
    Allow,
    Deny(Option<String>),
}

fn parse_decision(stdout: &str) -> Result<Decision, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Decision::Allow);
    }
    #[derive(Deserialize)]
    struct DecisionJson {
        decision: String,
        #[serde(default)]
        reason: Option<String>,
    }
    let parsed: DecisionJson =
        serde_json::from_str(text).map_err(|e| format!("invalid decision JSON: {e}"))?;
    match parsed.decision.as_str() {
        "allow" => Ok(Decision::Allow),
        "deny" => Ok(Decision::Deny(parsed.reason)),
        other => Err(format!("unknown decision {other:?}")),
    }
}

/// A deny without a reason names the hook; a reason is capped so a
/// runaway hook cannot flood the model's context.
fn deny_reason(reason: Option<String>, hook: &Hook) -> String {
    match reason.map(|r| r.trim().to_string()) {
        Some(reason) if !reason.is_empty() => cap_text(&reason, REASON_CAP),
        _ => format!("`{}`", hook.command),
    }
}
