use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    run_with(dir, args, &Cancel::none()).await
}

/// The same call with the user's stop request in hand.
async fn run_with(dir: &Path, args: Value, cancel: &Cancel) -> ToolResult {
    FindTool::new(dir.to_path_buf())
        .execute(&args, cancel)
        .await
}

/// A stop request raised from a real thread `delay` from now — the walk is
/// synchronous, so the flag cannot come from this task's executor.
fn stop_after(delay: std::time::Duration) -> (Cancel, std::thread::JoinHandle<()>) {
    let flag = Arc::new(AtomicBool::new(false));
    let setter = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            flag.store(true, Ordering::Release);
        })
    };
    (Cancel::new(flag), setter)
}

fn tree(dir: &Path) {
    std::fs::create_dir_all(dir.join("src/deep")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.join("src/deep/util.rs"), "").unwrap();
    std::fs::write(dir.join("README.md"), "x").unwrap();
    std::fs::create_dir_all(dir.join(".hidden")).unwrap();
    std::fs::write(dir.join(".hidden/secret.rs"), "").unwrap();
    std::fs::create_dir_all(dir.join("target/debug")).unwrap();
    std::fs::write(dir.join("target/debug/out.rs"), "").unwrap();
}

#[tokio::test]
async fn matches_names_by_glob_sorted_and_hidden_included_while_pruning_target() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(dir.path(), serde_json::json!({"pattern": "*.rs"})).await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output,
        ".hidden/secret.rs\nsrc/deep/util.rs\nsrc/main.rs\n"
    );
}

#[tokio::test]
async fn include_ignored_surfaces_target_and_gitignored_files() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "*.rs", "include_ignored": true}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("target/debug/out.rs"));
}

#[tokio::test]
async fn pathological_star_patterns_terminate_quickly() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a".repeat(200)), "").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "*a*a*a*a*a*a*a*a*b"}),
    )
    .await;
    assert!(!r.is_error);
    assert_eq!(r.output, "(no matches)\n");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_directory_root_is_searched() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let link = dir.path().join("link-src");
    std::os::unix::fs::symlink(dir.path().join("src"), &link).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "*.rs", "path": "link-src"}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("main.rs"), "{}", r.output);
}

#[tokio::test]
async fn invalid_glob_returns_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(dir.path(), serde_json::json!({"pattern": "[unclosed"})).await;
    assert!(r.is_error);
    assert!(r.output.contains("invalid glob"));
}

#[tokio::test]
async fn caps_results_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), "").unwrap();
    }

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "*.txt", "max_results": 4}),
    )
    .await;
    assert!(r.output.contains("[truncated: stopped after 4 matches"));
    assert_eq!(r.output.matches('\n').count(), 5);
}

/// Esc before the call: the walk never starts, and the answer says so as
/// an error rather than as a confident "(no matches)".
#[tokio::test]
async fn a_cancelled_search_reports_aborted_without_searching() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run_with(
        dir.path(),
        serde_json::json!({"pattern": "*.rs"}),
        &Cancel::new(Arc::new(AtomicBool::new(true))),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("aborted by user"), "{}", r.output);
}

/// Esc mid-walk: a stop request raised while the walk is in flight ends
/// it. The partial hit list is the proof it really stopped early: the same
/// call uncancelled finds every file.
#[tokio::test]
async fn a_cancelled_walk_stops_and_reports_partial_results() {
    let dir = tempfile::tempdir().unwrap();
    // Empty `.rs` files, ten to a directory, a cap above the tree's size.
    let args = |total: usize| serde_json::json!({"pattern": "*.rs", "max_results": total + 10});
    let (mut from, mut dirs) = (0, 1000);
    let (walk_time, total) = loop {
        for i in from..dirs {
            let sub = dir.path().join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            for j in 0..10 {
                std::fs::write(sub.join(format!("f{j}.rs")), "").unwrap();
            }
        }
        from = dirs;
        let total = dirs * 10;
        // Grown until an uncancelled walk takes a measurable while: a stop
        // timed at a quarter of it then lands inside the walk on any
        // machine, however fast or loaded.
        let measured = std::time::Instant::now();
        let complete = run(dir.path(), args(total)).await;
        let walk_time = measured.elapsed();
        assert!(!complete.is_error, "{}", complete.output);
        assert_eq!(complete.output.matches('\n').count(), total);
        if walk_time >= std::time::Duration::from_millis(200) || dirs >= 16000 {
            break (walk_time, total);
        }
        dirs *= 2;
    };

    let (cancel, setter) = stop_after(walk_time / 4);
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run_with(dir.path(), args(total), &cancel),
    )
    .await
    .expect("a cancelled walk must not run on to the end of the tree");
    setter.join().unwrap();

    assert!(r.is_error, "a stopped walk is an error: {}", r.output);
    assert!(r.output.contains("aborted by user"), "{}", r.output);
    assert!(r.output.contains("partial"), "{}", r.output);
    let found = r.output.lines().filter(|l| l.ends_with(".rs")).count();
    assert!(
        found > 0 && found < total,
        "the walk must stop partway: found {found} of {total}"
    );
}
