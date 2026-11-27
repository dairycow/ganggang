use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::output::{prefix_string, suffix_string};
use super::*;
use crate::nested::{DEPTH, MAX_TURNS_CAP, PARENT_MODEL, PARENT_SESSION, TOOL_CAP};

mod joblog;
mod jobreport;
mod jobs;

/// Tests that keep background jobs or touch the run's job log hold this
/// slot: the log file is named with the process id, so every tool in this
/// test binary shares one, and creating or dropping a tool rewrites it.
pub(super) static ONE_TOOL_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A nest with nothing in it: these tests are about commands, not nesting.
fn no_nest() -> SharedParentEnv {
    ParentEnv::default().shared()
}

/// A cancel flag nothing sets — the tests here are about running and
/// timing out, not about the user stopping a command.
fn no_cancel() -> Cancel {
    Cancel::none()
}

/// Job-log tests hold the shared slot and use one tool from start to
/// finish: every tool in the binary has a log of its own now, but the
/// kept-job tests still serialise on the exit kill below.
async fn one_log_test() -> tokio::sync::MutexGuard<'static, ()> {
    ONE_TOOL_AT_A_TIME.lock().await
}

/// A tool whose ceiling is `max`, for the job-log tests.
fn log_tool(max: u64) -> BashTool {
    BashTool::new(std::env::temp_dir(), max, no_nest())
}

/// Run `script` as a plain command through `tool`.
async fn run_on(tool: &BashTool, script: &str) -> ToolResult {
    tool.execute(&serde_json::json!({"command": script}), &no_cancel())
        .await
}

/// `wait_job: name`, waiting `budget_secs`.
async fn wait_job(tool: &BashTool, name: &str, budget_secs: Option<u64>) -> ToolResult {
    let mut args = serde_json::json!({"wait_job": name});
    if let Some(secs) = budget_secs {
        args["timeout_secs"] = serde_json::json!(secs);
    }
    tool.execute(&args, &no_cancel()).await
}

/// Register a job the way a command does: background a sleeper, write its
/// `start` record with `$!`, leave a line in `NAME.err`, and `extra`, a
/// `done` record, lands one second later from a background subshell. The
/// background children send their output to the void, so the command's
/// own pipe closes when the script ends and nothing kills them.
fn register(name: &str, body: &str, extra: Option<&str>) -> String {
    let mut script = format!(
        "sleep 5 >/dev/null 2>&1 & \
         printf 'start\\t{name}\\t%s\\t%s\\n' \"$!\" \"$PWD\" >> \"$GG_JOB_LOG\"; \
         printf '{body}\\n' > {name}.err"
    );
    if let Some(record) = extra {
        script.push_str(&format!(
            "; (sleep 1; printf '{record}\\n' >> \"$GG_JOB_LOG\") >/dev/null 2>&1 &"
        ));
    }
    script
}

async fn run(cmd: &str, timeout: Option<u64>) -> ToolResult {
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
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

/// A `timeout_secs` within the ceiling is the deadline as given: the
/// command dies there.
#[tokio::test]
async fn timeout_kills_the_command_at_the_requested_deadline() {
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
    let started = std::time::Instant::now();
    let r = tool
        .execute(
            &serde_json::json!({"command": "sleep 30", "timeout_secs": 1}),
            &no_cancel(),
        )
        .await;
    assert!(r.is_error);
    assert!(r.output.contains("timed out after 1s"));
    assert!(started.elapsed().as_secs() < 5, "kill must be prompt");
}

/// A `timeout_secs` over the ceiling is refused before any process
/// starts: the marker proves the command never ran, so no work is lost
/// to a clamp that would have killed it at the ceiling instead.
#[tokio::test]
async fn a_timeout_over_the_ceiling_is_refused_and_the_command_does_not_run() {
    refuse_over_ceiling(false).await;
}

/// The same refusal for a `background: true` call: the ceiling is final
/// there too, before a job is ever kept.
#[tokio::test]
async fn a_background_call_over_the_ceiling_is_refused_too() {
    refuse_over_ceiling(true).await;
}

/// The body of both refusal tests: `timeout_secs` 999 against a ceiling
/// of 1, a command that would touch `marker` and sleep, and the checks
/// that say it never did.
async fn refuse_over_ceiling(background: bool) {
    let marker = std::env::temp_dir().join(format!(
        "gg-bash-ceiling-refusal-{}-{}",
        std::process::id(),
        if background { "bg" } else { "fg" }
    ));
    let _ = std::fs::remove_file(&marker);
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
    let command = format!("touch {}; sleep 30", marker.display());
    let r = tool
        .execute(
            &serde_json::json!({
                "command": command,
                "timeout_secs": 999,
                "background": background,
            }),
            &no_cancel(),
        )
        .await;
    assert!(r.is_error);
    assert!(r.output.contains("did not run"), "{}", r.output);
    assert!(r.output.contains("999"), "{}", r.output);
    assert!(r.output.contains("ceiling of 1 s"), "{}", r.output);
    assert!(
        !r.output.contains("timed out"),
        "the command never ran, so nothing timed out: {}",
        r.output
    );
    assert!(!marker.exists(), "the command never started");
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group_not_just_the_shell() {
    // A pipeline means bash forks children; killing only bash would
    // leave them running past the deadline. The marker doubles as the
    // (unique, valid) sleep duration so pgrep can find group members.
    let pid = std::process::id();
    let marker = format!("617.{pid}");
    let pattern = format!(r"617\.{pid}([^0-9]|$)");
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
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
            .arg(&pattern)
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
    let pid = std::process::id();
    let marker = format!("731.{pid}");
    let pattern = format!(r"731\.{pid}([^0-9]|$)");
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
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
            .arg(&pattern)
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
    let tool = BashTool::new(std::env::temp_dir(), 300, no_nest());
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

#[test]
fn interior_non_utf8_bytes_do_not_truncate_head_or_tail() {
    let head = b"HEAD_START\xffHEAD_AFTER";
    assert!(
        prefix_string(head, OUTPUT_CAP / 4).contains("HEAD_AFTER"),
        "valid output after an interior bad byte must survive"
    );

    let tail = b"TAIL_BEFORE\xffTAIL_END";
    assert!(
        suffix_string(tail, OUTPUT_CAP - OUTPUT_CAP / 4).contains("TAIL_END"),
        "valid output after an interior bad byte must survive"
    );
}

#[tokio::test]
async fn an_endless_stream_is_bounded_and_reports_partial_output() {
    // /dev/zero never EOFs: the capped read loop keeps 10 KiB and
    // discards the rest until the timeout kills the group.
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
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
    let tool = BashTool::new(std::env::temp_dir(), 1, no_nest());
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
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
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

/// The nudge is decided from the command text alone: a pipe stays inside
/// one segment, quotes and `KEY=value` prefixes do not hide the command
/// word, and a redundant `cd` is appended after any tool workaround.
#[test]
fn workaround_hint_names_the_native_tool() {
    let cwd = Path::new("/work/dir");
    const READ: &str = "use native read (supports offset/limit and negative offset), outline or edit instead of bash";
    let hinted = [
        ("cat src/main.rs", READ),
        ("sed -n '1,20p' src/main.rs", READ),
        ("head -5 README.md", READ),
        ("grep -rn panic src/", "use native grep instead of bash"),
        ("find . -name '*.rs'", "use native find instead of bash"),
        ("ls -la src/", "use native ls or find instead of bash"),
        (
            "/usr/bin/python3 -c \"open('a.txt').write('x')\"",
            "use native read, edit or write instead of inline python in bash",
        ),
        (
            "cd /work/dir && echo ok",
            "bash already runs in the working directory; omit cd",
        ),
        (
            "cd '/work/dir/' && cat a.txt",
            &format!("{READ}; bash already runs in the working directory; omit cd"),
        ),
        ("FOO=1 BAR=2 grep x y", "use native grep instead of bash"),
    ];
    for (command, expected) in hinted {
        assert_eq!(
            hint::workaround_hint(command, cwd).as_deref(),
            Some(expected),
            "{command}"
        );
    }
    assert_eq!(
        hint::workaround_hint("ls -la ~/.gg; cat ~/.gg/config.toml", cwd).as_deref(),
        Some(
            "use native ls or find instead of bash; \
             use native read (supports offset/limit and negative offset), outline or edit instead of bash"
        )
    );
    assert_eq!(
        hint::workaround_hint("cat a; cat b", cwd).as_deref(),
        Some(READ)
    );
}

/// Normal commands, and pipelines whose tail is a coreutils tool, are left
/// alone: the nudge is for workarounds, not for ordinary shell use.
#[test]
fn workaround_hint_leaves_normal_commands_alone() {
    let cwd = Path::new("/work/dir");
    for command in [
        "echo hi",
        "cargo test | tail -20",
        "git diff | grep foo",
        "printf 'a\\nb\\n' | grep a",
        "python3 -c \"print(1+1)\"",
        "./run_checks.sh",
        "echo 'cat file'",
        "cd /other/dir && echo ok",
    ] {
        assert_eq!(
            hint::workaround_hint(command, cwd),
            None,
            "no hint expected for {command}"
        );
    }
}

/// A heredoc body is not shell code, but it is still the python program the
/// command runs, so file-touching code inside it counts.
#[test]
fn workaround_hint_reads_python_heredoc_bodies() {
    let cwd = Path::new("/work/dir");
    let command = "python3 - <<'PY'\nfrom pathlib import Path\nPath('a.txt').write_text('x')\nPY\n";
    assert_eq!(
        hint::workaround_hint(command, cwd).as_deref(),
        Some("use native read, edit or write instead of inline python in bash"),
        "{command}"
    );
}

/// A heredoc that only prints stays unhinted, and inline `perl -i` gets the
/// `edit` nudge while ordinary `perl` use does not.
#[test]
fn workaround_hint_separates_inline_perl_edits_from_ordinary_perl() {
    let cwd = Path::new("/work/dir");
    let hinted = [
        ("perl -0pi -e 's/foo/bar/' src/a.rs"),
        ("perl -pi.bak -e 's/foo/bar/' src/a.rs"),
        ("perl -i -pe 's/foo/bar/' src/a.rs"),
    ];
    for command in hinted {
        assert_eq!(
            hint::workaround_hint(command, cwd).as_deref(),
            Some("use native edit (supports replace_all: true) instead of inline perl in bash"),
            "{command}"
        );
    }
    for command in [
        "python3 - <<'PY'\nprint(1+1)\nPY\n",
        "python3 -c 'print(1)' && echo hi <<'EOF'\nPath('a')\nEOF\n",
        "perl -e 'print 1'",
        "perl -I/tmp/lib script.pl",
        "perl -i -I/tmp/expr script.pl",
    ] {
        assert_eq!(
            hint::workaround_hint(command, cwd),
            None,
            "no hint expected for {command}"
        );
    }
}

/// The tool appends the nudge after the output, keeping the exit code and
/// the error flag of the command itself.
#[tokio::test]
async fn a_workaround_command_gets_a_hint_line() {
    let cwd = std::env::temp_dir();
    let tool = BashTool::new(cwd.clone(), 30, no_nest());
    let cases = [
        ("cat /dev/null", "use native read"),
        ("sed '' /dev/null", "use native read"),
        ("head -n 1 /dev/null", "use native read"),
        ("grep a /dev/null; true", "use native grep"),
        ("find . -maxdepth 0", "use native find"),
        ("ls", "use native ls or find"),
        (
            "python3 -c \"open('/dev/null', 'w').write('x')\"",
            "use native read, edit or write",
        ),
        (
            &format!("cd {} && echo ok", cwd.display()),
            "bash already runs in the working directory; omit cd",
        ),
    ];
    for (command, hint) in cases {
        let r = tool
            .execute(&serde_json::json!({"command": command}), &no_cancel())
            .await;
        assert!(
            r.output.contains(&format!("[hint: {hint}")),
            "expected a hint for {command}: {}",
            r.output
        );
    }
}

/// Ordinary shell use gets no hint line at all.
#[tokio::test]
async fn an_ordinary_command_gets_no_hint() {
    let tool = BashTool::new(std::env::temp_dir(), 30, no_nest());
    for command in [
        "echo hi",
        "cargo test | tail -20",
        "printf 'a\\nb\\n' | grep a",
        "python3 -c \"print(1+1)\"",
    ] {
        let r = tool
            .execute(&serde_json::json!({"command": command}), &no_cancel())
            .await;
        assert!(
            !r.output.contains("[hint:"),
            "no hint expected for {command}: {}",
            r.output
        );
    }
}

#[test]
fn a_command_carries_the_whole_nest_set_or_removed() {
    let nest = ParentEnv {
        session: Some("parent-1".into()),
        tool_cap: Some(vec!["read".into()]),
        ..ParentEnv::default()
    };
    let mut command = tokio::process::Command::new("bash");
    apply_env(&mut command, nest.exports());
    let seen: BTreeMap<String, Option<String>> = command
        .as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(seen[DEPTH], Some("1".into()));
    assert_eq!(seen[PARENT_SESSION], Some("parent-1".into()));
    assert_eq!(seen[TOOL_CAP], Some("read".into()));
    // Nothing to say, so the variable goes: an inherited value must not
    // stand in for this run's.
    assert_eq!(seen[PARENT_MODEL], None);
    assert_eq!(seen[MAX_TURNS_CAP], None);
    assert_eq!(seen[crate::nested::MAX_SECONDS_CAP], None);
}

#[tokio::test]
async fn a_command_sees_the_nest_of_the_run_that_started_it() {
    let nest = ParentEnv {
        session: Some("parent-1".into()),
        tool_cap: Some(vec!["read".into(), "bash".into()]),
        max_turns_cap: Some(9),
        max_seconds_cap: Some(120),
        ..ParentEnv::default()
    };
    let tool = BashTool::new(std::env::temp_dir(), 30, nest.shared());
    let r = tool
        .execute(
            &serde_json::json!({"command": "printf '%s\\n' \"$GG_NEST_DEPTH|$GG_PARENT_SESSION|$GG_TOOL_CAP|$GG_MAX_TURNS_CAP|$GG_MAX_SECONDS_CAP\""}),
            &no_cancel(),
        )
        .await;
    assert!(
        r.output.contains("1|parent-1|read,bash|9|120"),
        "{}",
        r.output
    );
}
