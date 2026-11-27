//! The `/jobs` view: one line per kept background job and one per
//! registered job, joined by the bash tool's `jobs_report`.

use super::super::jobreport;
use super::*;

/// `/jobs` on a fresh tool: nothing held, nothing registered.
#[tokio::test]
async fn the_jobs_report_starts_at_no_jobs() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    assert_eq!(tool.jobs_report().as_deref(), Some("no jobs"));
}

/// `/jobs` lines: a kept background job by number and command, a
/// registered job running with its progress line, and one done.
#[tokio::test]
async fn the_jobs_report_lists_kept_and_registered_jobs() {
    let _log = one_log_test().await;
    let tool = log_tool(30);

    let r = tool
        .execute(
            &serde_json::json!({"command": "sleep 3", "background": true, "timeout_secs": 1}),
            &no_cancel(),
        )
        .await;
    assert!(r.output.contains("background job 1"), "{}", r.output);
    let report = tool.jobs_report().unwrap();
    assert!(report.contains("#1 running: sleep 3"), "{report}");

    let script = register("reg", "compiling src/main.rs", None);
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);
    let report = tool.jobs_report().unwrap();
    assert!(report.contains("reg · running"), "{report}");
    assert!(report.contains("pid "), "{report}");
    assert!(report.contains("compiling src/main.rs"), "{report}");

    let script = register("fin", "started", Some("done\tfin\t0\tlanded"));
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);
    let r = wait_job(&tool, "fin", Some(5)).await;
    assert!(!r.is_error, "{}", r.output);
    let report = tool.jobs_report().unwrap();
    assert!(report.contains("fin · done exit 0 · landed"), "{report}");
    assert!(report.contains("reg · running"), "{report}");

    // Collect the kept job: the line turns to its exit.
    let r = tool
        .execute(&serde_json::json!({"wait_for": 1}), &no_cancel())
        .await;
    assert!(!r.is_error, "{}", r.output);
    let report = tool.jobs_report().unwrap();
    assert!(report.contains("#1 exited: exit 0"), "{report}");
}

/// Long commands and progress lines are trimmed, at the cap, with an
/// ellipsis.
#[test]
fn jobs_report_trims_long_fields() {
    assert_eq!(
        jobreport::clamp_chars("short", 80),
        "short",
        "nothing to trim"
    );
    let long = "x".repeat(90);
    let trimmed = jobreport::clamp_chars(&long, 80);
    assert_eq!(
        trimmed.chars().count(),
        81,
        "80 characters plus the ellipsis"
    );
    assert!(trimmed.ends_with('…'));
}
