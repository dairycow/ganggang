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
async fn long_output_keeps_the_head_and_the_tail() {
    let r = run(
        "printf 'START'; printf 'x%.0s' {1..12000}; printf 'END'",
        None,
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("START"), "head must survive");
    assert!(r.output.contains("END"), "tail must survive");
    assert!(r.output.contains("bytes omitted"), "{}", r.output);
    assert!(
        r.output.len() < OUTPUT_CAP + 100,
        "capped with a marker: {}",
        r.output.len()
    );
}

/// A stream cut mid-character still joins on a char boundary instead of
/// producing a replacement character at the join.
#[tokio::test]
async fn long_multibyte_output_stays_valid_utf8() {
    let r = run("printf 'é%.0s' {1..7000}", None).await;
    assert!(!r.is_error);
    assert!(r.output.contains('é'));
    assert!(r.output.contains("bytes omitted"));
    assert!(r.output.len() < OUTPUT_CAP + 100, "{}", r.output.len());
}

#[tokio::test]
async fn an_endless_stream_is_bounded_and_reports_partial_output() {
    // /dev/zero never EOFs: the capped read loop keeps 10 KiB and
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
    assert!(
        r.output.len() < OUTPUT_CAP + 200,
        "bounded output: {}",
        r.output.len()
    );
    assert!(started.elapsed().as_secs() < 5);
}

#[tokio::test]
async fn timeout_reports_output_that_arrived_before_the_kill() {
    let tool = BashTool::new(std::env::temp_dir(), 1);
    let r = tool
        .execute(
            &serde_json::json!({"command": "printf 'before-timeout'; sleep 30"}),
            &no_cancel(),
        )
        .await;
    assert!(r.is_error);
    assert!(r.output.contains("timed out after 1s"));
    assert!(r.output.contains("[partial output]"), "{}", r.output);
    assert!(r.output.contains("before-timeout"), "{}", r.output);
}

#[tokio::test]
async fn cancellation_reports_output_that_arrived_before_the_kill() {
    let tool = BashTool::new(std::env::temp_dir(), 30);
    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            flag.store(true, Ordering::Relaxed);
        })
    };
    let r = tool
        .execute(
            &serde_json::json!({"command": "printf 'before-cancel'; sleep 30"}),
            &Cancel::new(flag),
        )
        .await;
    setter.await.unwrap();
    assert!(r.is_error);
    assert!(r.output.contains("aborted by user and killed"));
    assert!(r.output.contains("[partial output]"), "{}", r.output);
    assert!(r.output.contains("before-cancel"), "{}", r.output);
}
