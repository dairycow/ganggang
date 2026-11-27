//! The job log: records a detached job registers, `wait_job` waiting on
//! them by name, and the two wait budgets that outlast the bash ceiling.
//! The report side's tests are in [`jobreport`](super::jobreport)'s.

use std::time::Duration;

use super::super::joblog::{self, JobLog, JobState};
use super::*;

/// A start record and a done record after one second, parsed from a log
/// written by hand: the reader's contract without a shell in the middle.
#[tokio::test]
async fn records_parse_and_latest_wins() {
    let _log = one_log_test().await;
    let log = JobLog::create();
    std::fs::write(
        log.path(),
        "start\talpha\t42\t/work\n\
         not a record\n\
         done\talpha\t0\tall good\n\
         start\tbeta\t7\t/elsewhere\n\
         done\tgamma\t3\t\n\
         start\talpha\t99\t/again\n",
    )
    .unwrap();
    let jobs = joblog::read_all(&log);
    assert_eq!(jobs.len(), 3, "alpha, beta, gamma; junk skipped: {jobs:?}");
    assert_eq!(
        jobs["alpha"],
        JobState::Running {
            pid: 99,
            dir: "/again".into()
        },
        "a start after a done means the job started again"
    );
    assert_eq!(
        jobs["beta"],
        JobState::Running {
            pid: 7,
            dir: "/elsewhere".into()
        }
    );
    assert_eq!(
        jobs["gamma"],
        JobState::Finished {
            exit: Some(3),
            note: String::new(),
            dir: None
        },
        "a done with no start has no directory to name"
    );
}

/// A command sees the log on every call, foreground or kept.
#[tokio::test]
async fn every_command_carries_the_log_path() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let r = run_on(
        &tool,
        "test -n \"$GG_JOB_LOG\" && test -f \"$GG_JOB_LOG\" && echo yes",
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("yes"), "{}", r.output);
}

/// A finished job reports its exit, its note and where its files live.
#[tokio::test]
async fn wait_job_reports_a_finished_job() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let script = register("fin", "warming up", Some("done\tfin\t0\tall good"));
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);

    let r = wait_job(&tool, "fin", Some(5)).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output.contains("job fin finished: exit 0. all good"),
        "{}",
        r.output
    );
    assert!(r.output.contains("answer: "), "{}", r.output);
    assert!(r.output.contains("fin.out (0 bytes)"), "{}", r.output);
    assert!(r.output.contains("progress: "), "{}", r.output);
    assert!(r.output.contains("session: "), "{}", r.output);
    assert!(
        !r.output.contains("warming up"),
        "a pass carries no err tail: {}",
        r.output
    );
}

/// A non-zero exit is an error result that carries the last of the
/// job's `NAME.err`.
#[tokio::test]
async fn a_failed_job_is_an_error_with_its_progress_tail() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let script = register("bad", "the compile broke", Some("done\tbad\t3\tcrashed"));
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);

    let r = wait_job(&tool, "bad", Some(5)).await;
    assert!(r.is_error, "{}", r.output);
    assert!(
        r.output.contains("job bad finished: exit 3. crashed"),
        "{}",
        r.output
    );
    assert!(r.output.contains("[partial output]"), "{}", r.output);
    assert!(r.output.contains("the compile broke"), "{}", r.output);
}

/// A pid gone with no done record counts as finished, with the note.
#[tokio::test]
async fn a_pid_gone_without_a_done_record_counts_as_finished() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    // The pid is reaped before the record lands, so `kill -0` misses it.
    let script = "sleep 0.2 >/dev/null 2>&1 & p=$!; wait $p; printf 'start\\tgone\\t%s\\t%s\\n' \"$p\" \"$PWD\" >> \"$GG_JOB_LOG\"";
    let r = run_on(&tool, script).await;
    assert!(!r.is_error, "{}", r.output);

    let r = wait_job(&tool, "gone", Some(5)).await;
    assert!(r.is_error, "{}", r.output);
    assert!(
        r.output
            .contains("job gone finished: exit unknown. exited without a done record"),
        "{}",
        r.output
    );
}

/// A job still running when the budget passes is an ok result that names
/// the pid, the directory and the way back in.
#[tokio::test]
async fn a_wait_reports_a_job_still_running_and_keeps_it() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let script = register("slow", "halfway there", None);
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);

    let started = std::time::Instant::now();
    let r = wait_job(&tool, "slow", Some(1)).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output
            .contains("job slow is still running after 1s (pid "),
        "{}",
        r.output
    );
    assert!(r.output.contains("wait_job: slow"), "{}", r.output);
    assert!(r.output.contains("halfway there"), "{}", r.output);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the budget ends the call, not the job"
    );
}

/// The budget of a wait is its own: `timeout_secs` past the bash ceiling
/// is taken as given, clamped to 3600 s.
#[tokio::test]
async fn a_wait_budget_outlasts_the_bash_ceiling() {
    let _log = one_log_test().await;
    // Ceiling 1 s: a wait clamped to it would report still running.
    let tool = log_tool(1);
    let script = register("pat", "building", Some("done\tpat\t0\tfine"));
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);

    let r = wait_job(&tool, "pat", Some(4)).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output.contains("job pat finished: exit 0. fine"),
        "{}",
        r.output
    );
}

/// A stop request ends the wait, not the job; a later wait collects it.
#[tokio::test]
async fn a_stop_request_ends_a_wait_and_leaves_the_job() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let script = register("hold", "still going", Some("done\thold\t0\tlanded"));
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);

    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = Arc::clone(&flag);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            flag.store(true, Ordering::Relaxed);
        })
    };
    let r = tool
        .execute(&serde_json::json!({"wait_job": "hold"}), &Cancel::new(flag))
        .await;
    setter.await.unwrap();
    assert!(r.is_error, "{}", r.output);
    assert!(
        r.output
            .contains("wait for job hold aborted by user; the job keeps running"),
        "{}",
        r.output
    );

    let r = wait_job(&tool, "hold", Some(5)).await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output.contains("job hold finished: exit 0. landed"),
        "{}",
        r.output
    );
}

/// A name with no record is an error that names what is registered and
/// how a job registers.
#[tokio::test]
async fn an_unknown_name_lists_the_registered_jobs() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    let r = wait_job(&tool, "nothing", None).await;
    assert!(r.is_error, "{}", r.output);
    assert!(
        r.output.contains("no job named \"nothing\""),
        "{}",
        r.output
    );
    assert!(r.output.contains("no job is registered"), "{}", r.output);
    assert!(r.output.contains("GG_JOB_LOG"), "{}", r.output);

    let script = register("known", "here", None);
    let r = run_on(&tool, &script).await;
    assert!(!r.is_error, "{}", r.output);
    let r = wait_job(&tool, "nothing", None).await;
    assert!(r.is_error, "{}", r.output);
    assert!(
        r.output.contains("jobs registered in the job log: known"),
        "{}",
        r.output
    );
}

/// `wait_job` takes no command, and not `wait_for` or `stop`.
#[tokio::test]
async fn wait_job_takes_no_command_and_not_the_other_parameters() {
    let _log = one_log_test().await;
    let tool = log_tool(30);
    for args in [
        serde_json::json!({"wait_job": "a", "command": "echo hi"}),
        serde_json::json!({"wait_job": "a", "wait_for": 1}),
        serde_json::json!({"wait_job": "a", "stop": 1}),
    ] {
        let r = tool.execute(&args, &no_cancel()).await;
        assert!(r.is_error, "{args}");
        assert!(r.output.contains("no command"), "{}", r.output);
    }
    let r = wait_job(&tool, "never-registered", None).await;
    assert!(
        r.is_error,
        "a bare wait_job is a wait, and this name is not registered"
    );
}

/// The schema documents `wait_job` and both wait budgets.
#[tokio::test]
async fn the_schema_offers_wait_job_and_the_wait_budgets() {
    let _log = one_log_test().await;
    let tool = log_tool(45);
    let schema = tool.schema();
    let wait_job = schema["properties"]["wait_job"]["description"]
        .as_str()
        .unwrap();
    assert!(wait_job.contains("GG_JOB_LOG"), "{wait_job}");
    let timeout = schema["properties"]["timeout_secs"]["description"]
        .as_str()
        .unwrap();
    assert!(timeout.contains("600 s"), "{timeout}");
    assert!(timeout.contains("3600 s"), "{timeout}");
    assert!(
        tool.description().contains("wait_job"),
        "{}",
        tool.description()
    );
}
