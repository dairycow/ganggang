//! Background jobs: `background: true` keeps a command that outlives the
//! deadline, `wait_for` collects it, and dropping the tool kills what is
//! still running.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// Kept-job tests run one at a time: the exit kill below fires
/// process-wide, so a parallel test's job must not be in the registry
/// when it does.
static KEPT_JOB_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Hold the kept-job slot for the duration of one test.
async fn one_job_test() -> tokio::sync::MutexGuard<'static, ()> {
    KEPT_JOB_TESTS.lock().await
}

/// Start `command` as a job whose first wait ends after `timeout_secs`.
async fn start(tool: &BashTool, command: &str, timeout_secs: u64) -> ToolResult {
    tool.execute(
        &serde_json::json!({"command": command, "background": true, "timeout_secs": timeout_secs}),
        &no_cancel(),
    )
    .await
}

/// `wait_for: id`, waiting `timeout_secs` (the default when `None`).
async fn wait(tool: &BashTool, id: u64, timeout_secs: Option<u64>) -> ToolResult {
    tool.execute(
        &serde_json::json!({"wait_for": id, "timeout_secs": timeout_secs}),
        &no_cancel(),
    )
    .await
}

/// Require, within a few polls, that nothing matches `pattern` (a
/// `pgrep -f` regex for this test's sleepers) any more.
async fn require_dead(pattern: &str, what: &str) {
    for _ in 0..20 {
        let alive = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(pattern)
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("process-group members survived {what}");
}

/// A stop request raised `millis` after the caller starts waiting.
fn stop_after(millis: u64) -> (Cancel, tokio::task::JoinHandle<()>) {
    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = Arc::clone(&flag);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(millis)).await;
            flag.store(true, Ordering::Relaxed);
        })
    };
    (Cancel::new(flag), setter)
}

/// Inside the deadline a job is just a command: the same result, no job.
#[tokio::test]
async fn a_job_that_finishes_within_the_deadline_reports_like_any_command() {
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, "echo quick", 30).await;
    assert!(!r.is_error);
    assert!(r.output.contains("[exit code: 0]"), "{}", r.output);
    assert!(r.output.contains("quick"), "{}", r.output);
    assert!(!r.output.contains("job"), "{}", r.output);
    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("no background job 1"), "{}", r.output);
}

/// Past the deadline the call still ends in time and names the job it
/// kept; `wait_for` then returns what a finished command does, and keeps
/// doing so.
#[tokio::test]
async fn a_job_still_running_at_the_deadline_is_kept_and_can_be_collected() {
    let _jobs = one_job_test().await;
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let started = Instant::now();
    let r = start(&tool, "printf start; sleep 3; echo LATE", 1).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the call ends at the deadline: {:?}",
        started.elapsed()
    );
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("background job 1"), "{}", r.output);
    assert!(r.output.contains("wait_for: 1"), "{}", r.output);
    assert!(r.output.contains("start"), "{}", r.output);

    let r = wait(&tool, 1, None).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("[exit code: 0]"), "{}", r.output);
    assert!(r.output.contains("LATE"), "{}", r.output);

    let r = wait(&tool, 1, None).await;
    assert!(
        r.output.contains("LATE"),
        "a collected job stays readable: {}",
        r.output
    );
}

/// `wait_for` ends at its own deadline too, reporting the job as still
/// running with whatever it has printed.
#[tokio::test]
async fn wait_for_reports_a_job_still_running_with_its_output_so_far() {
    let _jobs = one_job_test().await;
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    start(&tool, "printf early; sleep 3; echo late", 1).await;
    let r = wait(&tool, 1, Some(1)).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("still running"), "{}", r.output);
    assert!(r.output.contains("wait_for: 1"), "{}", r.output);
    assert!(r.output.contains("early"), "{}", r.output);
    assert!(!r.output.contains("late"), "{}", r.output);

    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("late"), "{}", r.output);
}

/// A number no job has, a job number beside a command, and no command at
/// all are argument errors: nothing runs.
#[tokio::test]
async fn wait_for_rejects_an_unknown_job_and_a_command() {
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    for id in [0, 7, u64::MAX] {
        let r = tool
            .execute(&serde_json::json!({"wait_for": id}), &no_cancel())
            .await;
        assert!(r.is_error);
        assert!(
            r.output.contains(&format!("no background job {id}")),
            "{}",
            r.output
        );
    }
    let r = tool
        .execute(
            &serde_json::json!({"wait_for": 1, "command": "echo hi"}),
            &no_cancel(),
        )
        .await;
    assert!(r.is_error);
    assert!(r.output.contains("no command"), "{}", r.output);

    let r = tool.execute(&serde_json::json!({}), &no_cancel()).await;
    assert!(r.is_error);
    assert!(r.output.contains("needs a command"), "{}", r.output);
}

/// A plain command is still killed at the deadline, and its timeout is
/// where a model that skipped the schema learns what to pass next time.
#[tokio::test]
async fn a_command_that_times_out_is_told_about_jobs() {
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
    let r = tool
        .execute(
            &serde_json::json!({"command": "sleep 10", "timeout_secs": 999}),
            &no_cancel(),
        )
        .await;
    assert!(r.is_error);
    assert!(
        r.output.contains("timed out after 1s and was killed"),
        "{}",
        r.output
    );
    assert!(r.output.contains("background: true"), "{}", r.output);
}

/// The schema offers both parameters and names the real ceiling, so a model
/// with a longer job knows a plain call cannot run it.
#[test]
fn the_schema_offers_jobs_and_names_the_ceiling() {
    let tool = BashTool::new(std::env::temp_dir(), 45, no_nest());
    let schema = tool.schema();
    for name in ["command", "timeout_secs", "background", "wait_for", "stop"] {
        assert!(schema["properties"].get(name).is_some(), "{name}");
    }
    let timeout = schema["properties"]["timeout_secs"]["description"]
        .as_str()
        .unwrap();
    assert!(timeout.contains("45"), "{timeout}");
    assert!(
        schema.get("required").is_none(),
        "a wait_for call carries no command"
    );
}

/// A job lives as long as the tool: dropping it takes down every job still
/// running, whole process group included. The markers double as the
/// (unique, valid) sleep durations so pgrep can find group members.
#[tokio::test]
async fn dropping_the_tool_kills_the_jobs_it_kept() {
    let _jobs = one_job_test().await;
    let pid = std::process::id();
    let (first, second) = (format!("811.{pid}"), format!("812.{pid}"));
    let patterns = [
        format!(r"811\.{pid}([^0-9]|$)"),
        format!(r"812\.{pid}([^0-9]|$)"),
    ];
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, &format!("sleep {first} | sleep {first}"), 1).await;
    assert!(r.output.contains("background job 1"), "{}", r.output);
    let r = start(&tool, &format!("sleep {second} | sleep {second}"), 1).await;
    assert!(r.output.contains("background job 2"), "{}", r.output);
    // Both patterns match something, so the check below cannot pass on a
    // pattern that never matched.
    for pattern in &patterns {
        assert!(matches(pattern), "{pattern}");
    }

    drop(tool);
    for pattern in &patterns {
        require_dead(pattern, "the tool's drop").await;
    }
}

/// Whether `pgrep -f` finds a process for `pattern`.
fn matches(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Jobs are numbered from 1 in the order they are kept, and each is
/// collected under its own number.
#[tokio::test]
async fn jobs_are_numbered_from_1_in_the_order_kept() {
    let _jobs = one_job_test().await;
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, "sleep 2; echo one", 1).await;
    assert!(r.output.contains("background job 1"), "{}", r.output);
    let r = start(&tool, "sleep 2; echo two", 1).await;
    assert!(r.output.contains("background job 2"), "{}", r.output);

    let r = wait(&tool, 2, None).await;
    assert!(r.output.contains("two"), "{}", r.output);
    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("one"), "{}", r.output);
    let r = wait(&tool, 3, None).await;
    assert!(r.output.contains("no background job 3"), "{}", r.output);
}

/// A collected job that exited non-zero is an error result that still
/// carries its output and exit code, as for any command.
#[tokio::test]
async fn a_collected_job_with_a_nonzero_exit_is_an_error_with_its_output() {
    let _jobs = one_job_test().await;
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, "echo oops; sleep 2; exit 3", 1).await;
    assert!(r.output.contains("background job 1"), "{}", r.output);
    let r = wait(&tool, 1, None).await;
    assert!(r.is_error, "{}", r.output);
    assert!(r.output.contains("[exit code: 3]"), "{}", r.output);
    assert!(r.output.contains("oops"), "{}", r.output);
}

/// A command that cannot start keeps no job: the call fails as a plain one
/// would, and there is nothing to collect.
#[tokio::test]
async fn a_job_that_cannot_spawn_keeps_nothing() {
    let nowhere = std::env::temp_dir().join("bg-jobs-no-such-dir");
    let tool = BashTool::new(nowhere, 30, no_nest());
    let r = start(&tool, "echo hi", 30).await;
    assert!(r.is_error);
    assert!(r.output.contains("failed to spawn bash"), "{}", r.output);
    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("no background job 1"), "{}", r.output);
}

/// `stop: id` kills a kept job, whole process group included, and what it
/// printed stays readable. A job that had exited, or a number no job has,
/// is reported as such. The marker doubles as the (unique, valid) sleep
/// duration so pgrep can find group members.
#[tokio::test]
async fn stop_kills_a_kept_job_and_leaves_a_finished_one_alone() {
    let _jobs = one_job_test().await;
    let pid = std::process::id();
    let marker = format!("831.{pid}");
    let pattern = format!(r"831\.{pid}([^0-9]|$)");
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(
        &tool,
        &format!("printf early; sleep {marker} | sleep {marker}"),
        1,
    )
    .await;
    assert!(r.output.contains("background job 1"), "{}", r.output);

    let r = tool
        .execute(&serde_json::json!({"stop": 1}), &no_cancel())
        .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("stopped"), "{}", r.output);
    require_dead(&pattern, "stop").await;

    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("early"), "{}", r.output);

    let r = tool
        .execute(&serde_json::json!({"stop": 1}), &no_cancel())
        .await;
    assert!(r.output.contains("already exited"), "{}", r.output);

    let r = tool
        .execute(&serde_json::json!({"stop": 9}), &no_cancel())
        .await;
    assert!(r.is_error);
    assert!(r.output.contains("no background job 9"), "{}", r.output);
}

/// A job number beside a command, or `wait_for` beside `stop`, is an
/// argument error: nothing runs.
#[tokio::test]
async fn a_job_number_takes_no_command_and_not_the_other_parameter() {
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let marker = std::env::temp_dir().join(format!("bg-arg-marker-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let touch = format!("touch {}", marker.display());
    for args in [
        serde_json::json!({"stop": 1, "command": touch}),
        serde_json::json!({"wait_for": 1, "command": "echo hi", "background": true}),
        serde_json::json!({"wait_for": 1, "stop": 1}),
    ] {
        let r = tool.execute(&args, &no_cancel()).await;
        assert!(r.is_error, "{args}");
        assert!(r.output.contains("no command"), "{}", r.output);
    }
    assert!(!marker.exists(), "a refused call must not run its command");
}

/// A job whose shell has exited is no longer signalled: by then its group
/// id may belong to another process. `setsid` puts the sleep in a session
/// of its own, holding the pipe, so the shell exits at once and the job is
/// kept while the sleep runs on. The marker rides in a second command, so
/// `sh` keeps it on its command line; the sleep ends by itself.
#[tokio::test]
async fn a_job_whose_shell_has_exited_is_not_signalled() {
    let _jobs = one_job_test().await;
    if std::process::Command::new("setsid")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return;
    }
    let marker = format!("841.{}", std::process::id());
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, &format!("setsid sh -c 'sleep 3; : {marker}'"), 1).await;
    assert!(r.output.contains("background job 1"), "{}", r.output);

    let r = tool
        .execute(&serde_json::json!({"stop": 1}), &no_cancel())
        .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output.contains("shell has already exited"),
        "{}",
        r.output
    );
    let alive = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(&marker)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(alive, "a job whose shell exited must not be signalled");

    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("exit code"), "{}", r.output);
}

/// Esc while a job is still inside its first wait kills it as it would any
/// command. The marker doubles as the (unique, valid) sleep duration so
/// pgrep can find group members.
#[tokio::test]
async fn a_stop_request_in_the_first_wait_kills_the_job() {
    let _jobs = one_job_test().await;
    let pid = std::process::id();
    let marker = format!("821.{pid}");
    let pattern = format!(r"821\.{pid}([^0-9]|$)");
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let (cancel, setter) = stop_after(100);
    let r = tool
        .execute(
            &serde_json::json!({
                "command": format!("sleep {marker} | sleep {marker}"),
                "background": true,
            }),
            &cancel,
        )
        .await;
    setter.await.unwrap();

    assert!(r.is_error);
    assert!(
        r.output.contains("command aborted by user and killed"),
        "{}",
        r.output
    );
    require_dead(&pattern, "the stop request").await;
    let r = wait(&tool, 1, None).await;
    assert!(
        r.output.contains("no background job 1"),
        "a job killed before it was kept is kept by no number: {}",
        r.output
    );
}

/// Once kept, a job outlives a stop request: the wait ends at once, and
/// the job is still there to collect.
#[tokio::test]
async fn a_stop_request_ends_a_wait_and_leaves_the_job_running() {
    let _jobs = one_job_test().await;
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    start(&tool, "sleep 3; echo done", 1).await;
    let (cancel, setter) = stop_after(100);
    let started = Instant::now();
    let r = tool
        .execute(&serde_json::json!({"wait_for": 1}), &cancel)
        .await;
    setter.await.unwrap();

    assert!(r.is_error);
    assert!(r.output.contains("aborted by user"), "{}", r.output);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "no waiting on the deadline"
    );
    let r = wait(&tool, 1, None).await;
    assert!(r.output.contains("done"), "{}", r.output);
}

/// `process::exit` runs no destructors, so the exit path carries the group
/// kill itself: the registry takes down a kept job with no tool drop in
/// sight. The tool is forgotten rather than dropped, so the exit kill is
/// the only thing that could have worked. The marker doubles as the
/// (unique, valid) sleep duration so pgrep can find group members.
#[tokio::test]
async fn the_exit_kill_takes_down_a_kept_job_without_a_drop() {
    let _jobs = one_job_test().await;
    let pid = std::process::id();
    let marker = format!("851.{pid}");
    let pattern = format!(r"851\.{pid}([^0-9]|$)");
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    let r = start(&tool, &format!("sleep {marker} | sleep {marker}"), 1).await;
    assert!(r.output.contains("background job 1"), "{}", r.output);
    assert!(matches(&pattern), "{pattern}");

    let killed = kill_jobs_at_exit();
    assert!(killed >= 1, "the exit kill signalled {killed} groups");
    std::mem::forget(tool);
    require_dead(&pattern, "the exit kill").await;
}
