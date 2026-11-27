//! Unit tests for config loading, the JSON decision, and the bounded
//! one-shot runner (§22). Loop wiring lives in `agent::loop_::tests::hooks`.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::agent::session::ToolCall;
use crate::agent::tools::ToolResult;

fn hook(event: HookEvent, tools: Option<Vec<&str>>, command: &str) -> Hook {
    Hook {
        event,
        tools: tools.map(|tools| tools.into_iter().map(str::to_string).collect()),
        command: command.to_string(),
        timeout_secs: DEFAULT_TIMEOUT_SECS,
    }
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "call_1".into(),
        name: name.into(),
        args,
        args_error: None,
    }
}

fn write_file(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("hooks.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn abort() -> AtomicBool {
    AtomicBool::new(false)
}

/// A project hooks file in `dir`, saying so in its command.
fn project_file(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir.join(".gg")).unwrap();
    let path = dir.join(".gg").join("hooks.toml");
    std::fs::write(&path, body).unwrap();
    path
}

const ONE_HOOK: &str = "[[hooks]]\nevent = \"run_end\"\ncommand = \"notify.sh\"\n";

#[test]
fn a_missing_file_means_no_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.toml");

    let (hooks, warnings) = load_from(Some(&path), dir.path(), &[], false).unwrap();

    assert!(hooks.is_empty());
    assert!(!hooks.disabled());
    assert!(warnings.is_empty());
}

/// A project file is executable behaviour a clone can carry: it stays
/// unloaded unless the user trusts its directory, and the warning says
/// how to.
#[test]
fn an_untrusted_project_file_is_left_unloaded_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let file = project_file(dir.path(), ONE_HOOK);

    let (hooks, warnings) = load_from(
        Some(&dir.path().join("absent.toml")),
        dir.path(),
        &[],
        false,
    )
    .unwrap();

    assert!(hooks.is_empty());
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0];
    assert!(warning.contains("project hooks"), "{warning}");
    assert!(warning.contains(&file.display().to_string()), "{warning}");
    assert!(warning.contains("trusted_projects"), "{warning}");
    assert_eq!(hooks.ignored(), warnings.as_slice(), "`/hooks` says it too");
}

#[test]
fn no_hooks_skips_loading_and_the_project_warning() {
    let dir = tempfile::tempdir().unwrap();
    project_file(dir.path(), "bogus");
    let bad = write_file(dir.path(), "this is not toml");

    let (hooks, warnings) =
        load_from(Some(&bad), dir.path(), &[dir.path().to_path_buf()], true).unwrap();

    assert!(hooks.disabled());
    assert!(hooks.is_empty());
    assert!(warnings.is_empty());
    assert!(hooks.ignored().is_empty());
}

/// The user's hooks come first, the trusted project's after them, so a
/// user's `pre_tool` guard is ahead of the project's in the line where the
/// first denial wins.
#[test]
fn a_trusted_project_file_loads_after_the_user_file() {
    let dir = tempfile::tempdir().unwrap();
    let user = write_file(dir.path(), ONE_HOOK);
    project_file(
        dir.path(),
        "[[hooks]]\nevent = \"pre_tool\"\ncommand = \"./scripts/gate.sh\"\n",
    );

    let (hooks, warnings) =
        load_from(Some(&user), dir.path(), &[dir.path().to_path_buf()], false).unwrap();

    assert!(warnings.is_empty());
    let commands: Vec<&str> = hooks.specs().iter().map(|h| h.command.as_str()).collect();
    assert_eq!(commands, ["notify.sh", "./scripts/gate.sh"]);
    assert!(!hooks.is_project(0), "the user's");
    assert!(hooks.is_project(1), "the project's");
}

/// The walk is `AGENTS.md`'s: the repo root down to the working directory,
/// broadest first, and no further up than the repo root. Trust in the root
/// covers the directories under it.
#[test]
fn project_files_walk_from_the_repo_root_to_cwd_under_one_trust() {
    let above = tempfile::tempdir().unwrap();
    let root = above.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    project_file(above.path(), ONE_HOOK);
    project_file(
        &root,
        "[[hooks]]\nevent = \"run_end\"\ncommand = \"root\"\n",
    );
    project_file(
        &root.join("sub"),
        "[[hooks]]\nevent = \"run_end\"\ncommand = \"sub\"\n",
    );

    let trusted = [root.clone()];
    let (hooks, warnings) = load_from(None, &root.join("sub"), &trusted, false).unwrap();

    assert!(
        warnings.is_empty(),
        "the file above the repo is not even found"
    );
    let commands: Vec<&str> = hooks.specs().iter().map(|h| h.command.as_str()).collect();
    assert_eq!(commands, ["root", "sub"], "broadest first");

    // Run from the root itself: its file alone.
    let (hooks, _) = load_from(None, &root, &trusted, false).unwrap();
    let commands: Vec<&str> = hooks.specs().iter().map(|h| h.command.as_str()).collect();
    assert_eq!(commands, ["root"]);
}

/// A trust listing is compared on real paths: a project reached through
/// a symlink loads under either spelling of its directory.
#[cfg(unix)]
#[test]
fn trust_is_compared_on_canonical_paths() {
    let dir = tempfile::tempdir().unwrap();
    project_file(dir.path(), ONE_HOOK);
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("link");
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();

    let (hooks, warnings) =
        load_from(None, &link.join("sub"), &[dir.path().to_path_buf()], false).unwrap();
    assert!(
        warnings.is_empty(),
        "trusted under the real path: {warnings:?}"
    );
    assert_eq!(hooks.specs().len(), 1);

    let (hooks, warnings) = load_from(None, &link, std::slice::from_ref(&link), false).unwrap();
    assert!(warnings.is_empty(), "trusted under the link: {warnings:?}");
    assert_eq!(hooks.specs().len(), 1);
}

/// A trusted file is config like the user's: a broken one is a startup
/// error that names it.
#[test]
fn a_broken_trusted_project_file_is_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = project_file(dir.path(), "[[hooks]]\nevent = \"nope\"\ncommand = \"x\"\n");

    let err = load_from(None, dir.path(), &[dir.path().to_path_buf()], false).unwrap_err();

    assert!(
        format!("{err:#}").contains(&file.display().to_string()),
        "{err:#}"
    );
}

#[test]
fn a_full_file_parses_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(
        dir.path(),
        r#"
[[hooks]]
event = "pre_tool"
tools = ["bash", "edit"]
command = "guard.sh"
timeout_secs = 5

[[hooks]]
event = "run_end"
command = "notify.sh"
"#,
    );

    let (hooks, warnings) = load_from(Some(&path), dir.path(), &[], false).unwrap();

    assert!(warnings.is_empty());
    assert_eq!(
        hooks.specs(),
        &[
            Hook {
                event: HookEvent::PreTool,
                tools: Some(vec!["bash".into(), "edit".into()]),
                command: "guard.sh".into(),
                timeout_secs: 5,
            },
            Hook {
                event: HookEvent::RunEnd,
                tools: None,
                command: "notify.sh".into(),
                timeout_secs: DEFAULT_TIMEOUT_SECS,
            },
        ]
    );
}

#[test]
fn invalid_configs_are_named_errors() {
    let cases = [
        (
            "unknown event",
            "[[hooks]]\nevent = \"pre_commit\"\ncommand = \"x\"\n",
        ),
        (
            "unknown tool",
            "[[hooks]]\nevent = \"pre_tool\"\ntools = [\"ghost\"]\ncommand = \"x\"\n",
        ),
        (
            "empty tools",
            "[[hooks]]\nevent = \"pre_tool\"\ntools = []\ncommand = \"x\"\n",
        ),
        (
            "tools on run_end",
            "[[hooks]]\nevent = \"run_end\"\ntools = [\"*\"]\ncommand = \"x\"\n",
        ),
        (
            "zero timeout",
            "[[hooks]]\nevent = \"run_end\"\ncommand = \"x\"\ntimeout_secs = 0\n",
        ),
        (
            "empty command",
            "[[hooks]]\nevent = \"run_end\"\ncommand = \"  \"\n",
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (what, body) in cases {
        let path = write_file(dir.path(), body);
        let err = load_from(Some(&path), dir.path(), &[], false).unwrap_err();
        assert!(
            err.to_string().contains("hooks.toml"),
            "{what}: unexpected error {err:#}"
        );
    }
}

#[test]
fn malformed_toml_is_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_file(dir.path(), "[[hooks]\n");
    let err = load_from(Some(&path), dir.path(), &[], false).unwrap_err();
    assert!(err.to_string().contains("parsing"), "{err:#}");
}

#[tokio::test]
async fn a_pre_hook_receives_the_documented_payload() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "cat > payload.json")]);

    let outcome = hooks
        .pre_tool(
            &call("bash", json!({"command": "git status"})),
            Some("session-1"),
            dir.path(),
            &abort(),
        )
        .await;
    assert!(matches!(outcome, PreOutcome::Allow { warnings } if warnings.is_empty()));

    let seen: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("payload.json")).unwrap())
            .unwrap();
    assert_eq!(
        seen,
        json!({
            "version": 1,
            "event": "pre_tool",
            "cwd": dir.path().display().to_string(),
            "session_id": "session-1",
            "tool": {"id": "call_1", "name": "bash", "args": {"command": "git status"}},
        })
    );
}

#[tokio::test]
async fn a_post_hook_receives_the_result_and_a_run_end_hook_the_turn_count() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Hooks::new(vec![
        hook(HookEvent::PostTool, None, "cat > post.json"),
        hook(HookEvent::RunEnd, None, "cat > end.json"),
    ]);

    let warnings = hooks
        .post_tool(
            &call("read", json!({"path": "x"})),
            &ToolResult::ok("contents".into()),
            None,
            dir.path(),
            &abort(),
        )
        .await;
    assert!(warnings.is_empty());
    let post: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("post.json")).unwrap())
            .unwrap();
    assert_eq!(post["event"], "post_tool");
    assert_eq!(
        post["result"],
        json!({"output": "contents", "is_error": false})
    );
    assert_eq!(post["session_id"], Value::Null);

    let warnings = hooks.run_end(3, None, dir.path(), &abort()).await;
    assert!(warnings.is_empty());
    let end: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("end.json")).unwrap())
            .unwrap();
    assert_eq!(end["event"], "run_end");
    assert_eq!(end["turns"], 3);
    assert!(end.get("tool").is_none());
    assert!(end.get("result").is_none());
}

#[tokio::test]
async fn a_deny_decision_is_returned_with_its_reason() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        Some(vec!["bash"]),
        r#"printf '{"decision":"deny","reason":"no recursive deletes"}'"#,
    )]);

    let outcome = hooks
        .pre_tool(
            &call("bash", json!({"command": "rm -rf /"})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;

    match outcome {
        PreOutcome::Deny { reason, warnings } => {
            assert_eq!(reason, "no recursive deletes");
            assert!(warnings.is_empty());
        }
        other => panic!("expected deny, got {other:?}"),
    }
}

#[tokio::test]
async fn a_deny_without_a_reason_names_the_hook() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        r#"printf '{"decision":"deny"}'"#,
    )]);

    match hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Deny { reason, .. } => assert!(reason.contains("printf")),
        other => panic!("expected deny, got {other:?}"),
    }
}

#[tokio::test]
async fn the_first_deny_wins_and_matching_is_exact() {
    let hooks = Hooks::new(vec![
        hook(
            HookEvent::PreTool,
            Some(vec!["bash"]),
            r#"printf '{"decision":"deny","reason":"first"}'"#,
        ),
        hook(
            HookEvent::PreTool,
            Some(vec!["edit"]),
            r#"printf '{"decision":"deny","reason":"second"}'"#,
        ),
    ]);

    match hooks
        .pre_tool(
            &call("bash", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Deny { reason, .. } => assert_eq!(reason, "first"),
        other => panic!("expected deny, got {other:?}"),
    }

    // A hook scoped to bash never sees a read call.
    let read = hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;
    assert!(matches!(read, PreOutcome::Allow { .. }));
}

#[tokio::test]
async fn malformed_output_and_nonzero_exits_warn_and_allow() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        r#"echo why >&2; printf 'not json'"#,
    )]);
    match hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Allow { warnings } => {
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("invalid"));
            assert!(warnings[0].contains("why"), "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }

    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        "echo boom >&2; exit 3",
    )]);
    match hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Allow { warnings } => {
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("exit 3"));
            assert!(warnings[0].contains("boom"));
        }
        other => panic!("expected allow, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unknown_decision_is_a_failure_not_a_deny() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        r#"printf '{"decision":"maybe"}'"#,
    )]);
    match hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Allow { warnings } => {
            assert!(warnings[0].contains("unknown decision"));
        }
        other => panic!("expected allow, got {other:?}"),
    }
}

#[tokio::test]
async fn a_hook_that_ignores_stdin_is_not_a_failure() {
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "true")]);
    let outcome = hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;
    assert!(matches!(outcome, PreOutcome::Allow { warnings } if warnings.is_empty()));
}

#[tokio::test]
async fn a_timeout_is_a_warning_and_the_run_continues() {
    let mut slow = hook(HookEvent::PreTool, None, "sleep 5");
    slow.timeout_secs = 1;
    let hooks = Hooks::new(vec![slow]);

    let started = Instant::now();
    let outcome = hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;

    match outcome {
        PreOutcome::Allow { warnings } => {
            assert!(warnings[0].contains("timed out after 1s"), "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }
    assert!(started.elapsed().as_secs() < 4, "kill must be prompt");
}

#[tokio::test]
async fn abort_interrupts_a_running_hook_and_its_children() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        &format!("sleep 37.123 & echo $! > {}; wait", pidfile.display()),
    )]);
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        flag.store(true, Ordering::Relaxed);
    });

    let started = Instant::now();
    let outcome = hooks
        .pre_tool(&call("read", json!({})), None, dir.path(), &abort)
        .await;

    assert!(matches!(outcome, PreOutcome::Interrupted));
    assert!(started.elapsed().as_secs() < 2, "abort must kill promptly");
    assert_dead(&wait_for_pid(&pidfile).await).await;
}

#[tokio::test]
async fn a_timeout_kills_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let mut slow = hook(
        HookEvent::PreTool,
        None,
        &format!("sleep 37.123 & echo $! > {}; wait", pidfile.display()),
    );
    slow.timeout_secs = 1;
    let hooks = Hooks::new(vec![slow]);

    let started = Instant::now();
    let outcome = hooks
        .pre_tool(&call("read", json!({})), None, dir.path(), &abort())
        .await;

    match outcome {
        PreOutcome::Allow { warnings } => {
            assert!(warnings[0].contains("timed out after 1s"), "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }
    assert!(started.elapsed().as_secs() < 4, "kill must be prompt");
    assert_dead(&wait_for_pid(&pidfile).await).await;
}

#[tokio::test]
async fn post_and_run_end_failures_only_warn() {
    let hooks = Hooks::new(vec![
        hook(HookEvent::PostTool, None, "exit 2"),
        hook(HookEvent::RunEnd, None, "exit 2"),
    ]);

    let warnings = hooks
        .post_tool(
            &call("read", json!({})),
            &ToolResult::ok("x".into()),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("post_tool hook"));

    let warnings = hooks
        .run_end(1, None, &std::env::temp_dir(), &abort())
        .await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("run_end hook"));
}

#[tokio::test]
async fn a_huge_stdout_is_drained_and_treated_as_invalid() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        "head -c 100000 /dev/zero | tr '\\0' a",
    )]);
    let started = Instant::now();
    let outcome = hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;
    match outcome {
        PreOutcome::Allow { warnings } => {
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("invalid output"), "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }
    assert!(started.elapsed().as_secs() < 5, "the pipe must be drained");
}

#[tokio::test]
async fn stderr_warnings_keep_the_end_of_the_stream() {
    let hooks = Hooks::new(vec![hook(
        HookEvent::PreTool,
        None,
        "printf 'THE-START' >&2; head -c 9000 /dev/zero | tr '\\0' x >&2; \
         printf 'THE-END' >&2; exit 1",
    )]);
    match hooks
        .pre_tool(
            &call("read", json!({})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await
    {
        PreOutcome::Allow { warnings } => {
            let tail = warnings[0]
                .split_once("exit 1:")
                .map(|(_, tail)| tail)
                .unwrap_or_default();
            assert!(tail.contains("THE-END"), "{warnings:?}");
            assert!(!tail.contains("THE-START"), "{warnings:?}");
            assert!(warnings[0].len() < 2000, "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }
}

#[tokio::test]
async fn a_spawn_failure_warns_and_allows() {
    let missing = std::env::temp_dir().join("gg-hook-missing-dir");
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "true")]);
    match hooks
        .pre_tool(&call("read", json!({})), None, &missing, &abort())
        .await
    {
        PreOutcome::Allow { warnings } => {
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("failed to spawn"), "{warnings:?}");
        }
        other => panic!("expected allow, got {other:?}"),
    }
}

#[tokio::test]
async fn a_payload_larger_than_the_pipe_is_not_a_failure() {
    let big = "x".repeat(200 * 1024);
    let hooks = Hooks::new(vec![hook(HookEvent::PreTool, None, "true")]);
    let outcome = hooks
        .pre_tool(
            &call("write", json!({"content": big})),
            None,
            &std::env::temp_dir(),
            &abort(),
        )
        .await;
    assert!(matches!(outcome, PreOutcome::Allow { warnings } if warnings.is_empty()));
}

#[tokio::test]
async fn dropping_the_hook_future_kills_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let hook = hook(
        HookEvent::PreTool,
        None,
        &format!("sleep 37.123 & echo $! > {}; wait", pidfile.display()),
    );
    let hooks = Hooks::new(vec![hook]);

    let tc = call("read", json!({}));
    let flag = abort();
    let future = hooks.pre_tool(&tc, None, dir.path(), &flag);
    let dropped = tokio::time::timeout(Duration::from_millis(500), future).await;
    assert!(dropped.is_err(), "the hook should still be running");

    assert_dead(&wait_for_pid(&pidfile).await).await;
}

async fn wait_for_pid(path: &Path) -> String {
    for _ in 0..40 {
        if let Ok(text) = std::fs::read_to_string(path) {
            let pid = text.trim().to_string();
            if !pid.is_empty() {
                return pid;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the hook never wrote its child pid");
}

async fn assert_dead(pid: &str) {
    for _ in 0..40 {
        if !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("grandchild {pid} survived its hook");
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn caps_cut_at_char_boundaries() {
    let text = "ééé";
    let capped = cap_text(text, 3);
    assert!(capped.starts_with("é"), "{capped}");
    assert!(capped.contains("truncated at 3 bytes"));
    assert_eq!(cap_text("ok", 10), "ok");
}
