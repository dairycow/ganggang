use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    run_with(dir, args, &Cancel::none()).await
}

/// The same call with the user's stop request in hand.
async fn run_with(dir: &std::path::Path, args: Value, cancel: &Cancel) -> ToolResult {
    GrepTool::new(dir.to_path_buf())
        .execute(&args, cancel)
        .await
}

fn tree(dir: &std::path::Path) {
    std::fs::write(
        dir.join("top.txt"),
        "alpha needle one\nnothing\nneedle two\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
    std::fs::write(dir.join("sub/mid.txt"), "needle three\n").unwrap();
    std::fs::write(dir.join("sub/deep/low.txt"), "the needle four\n").unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/config"), "needle in git\n").unwrap();
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("target/build.rs"), "needle in target\n").unwrap();
}

#[tokio::test]
async fn matches_across_a_tree_sorted_and_grouped_by_file() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(dir.path(), serde_json::json!({"pattern": "needle"})).await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output,
        "sub/deep/low.txt:\n\
         \x20 1: the needle four\n\
         sub/mid.txt:\n\
         \x20 1: needle three\n\
         top.txt:\n\
         \x20 1: alpha needle one\n\
         \x20 3: needle two\n"
    );
}

#[tokio::test]
async fn supports_regex_case_insensitive_fixed_strings_glob_and_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one\nAlphaNeedle\nthree\nfoo[0]\n").unwrap();
    std::fs::write(dir.path().join("b.txt"), "AlphaNeedle\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({
            "pattern": "alpha.*needle|missing",
            "case_insensitive": true,
            "glob": "*.rs",
            "context": 1
        }),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(r.output, "a.rs:\n  1- one\n  2: AlphaNeedle\n  3- three\n");

    let r_fixed = run(
        dir.path(),
        serde_json::json!({"pattern": "foo[0]", "fixed_strings": true}),
    )
    .await;
    assert!(!r_fixed.is_error);
    assert_eq!(r_fixed.output, "a.rs:\n  4: foo[0]\n");
}

#[tokio::test]
async fn invalid_regex_returns_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(dir.path(), serde_json::json!({"pattern": "[unclosed"})).await;
    assert!(r.is_error);
    assert!(r.output.contains("invalid regex"));
}

#[tokio::test]
async fn respects_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "needle\n").unwrap();
    std::fs::write(dir.path().join("kept.txt"), "needle\n").unwrap();

    let r = run(dir.path(), serde_json::json!({"pattern": "needle"})).await;
    assert!(!r.is_error);
    assert_eq!(r.output, "kept.txt:\n  1: needle\n");
}

#[tokio::test]
async fn no_match_is_a_clean_not_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "nothing here\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle", "path": "."}),
    )
    .await;
    assert!(!r.is_error);
    assert_eq!(r.output, "(no matches in 1 file(s))\n");
}

#[tokio::test]
async fn caps_results_with_a_visible_stop_notice() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..20).map(|i| format!("hit {i}\n")).collect();
    std::fs::write(dir.path().join("many.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "hit", "path": ".", "max_results": 5}),
    )
    .await;
    assert!(!r.is_error);
    assert_eq!(r.output.match_indices("hit").count(), 5);
    assert!(r.output.contains("[truncated: stopped after 5 matches"));
}

#[tokio::test]
async fn output_is_capped_at_16_kib_with_a_visible_notice() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..500)
        .map(|i| format!("needle {i} {}\n", "x".repeat(100)))
        .collect();
    std::fs::write(dir.path().join("wide.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle", "max_results": 10000}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output
            .contains("output capped at 16384 bytes — narrow pattern or path"),
        "{}",
        r.output.lines().last().unwrap_or("")
    );
    assert!(
        r.output.len() < MAX_OUTPUT_BYTES + 200,
        "hard byte cap: {}",
        r.output.len()
    );
}

#[test]
fn a_byte_capped_first_file_does_not_claim_no_matches() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = Cancel::none();
    let mut search = super::search::Search::new(
        dir.path(),
        regex::Regex::new("needle").unwrap(),
        None,
        0,
        50,
        false,
        &cancel,
    );
    // The state a walk reaches when its first output line did not fit.
    search.force_byte_capped();

    let r = search.report();
    assert!(!r.is_error, "{}", r.output);
    assert!(!r.output.contains("(no matches"), "{}", r.output);
    assert!(r.output.contains("output capped"), "{}", r.output);
}

#[tokio::test]
async fn files_only_reports_each_file_with_its_match_count() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle", "files_only": true}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output,
        "sub/deep/low.txt (1)\nsub/mid.txt (1)\ntop.txt (2)\n"
    );
}

#[tokio::test]
async fn binary_files_are_skipped_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("text.txt"), "needle in text\n").unwrap();
    std::fs::write(dir.path().join("blob.bin"), b"needle\x00binary").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle", "path": "."}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("text.txt"));
    assert!(!r.output.contains("blob.bin:"));
    assert!(r.output.contains("(1 binary file(s) skipped)"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_directory_root_is_searched() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());
    let link = dir.path().join("link-sub");
    std::os::unix::fs::symlink(dir.path().join("sub"), &link).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle three", "path": "link-sub"}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("link-sub/mid.txt:"), "{}", r.output);
    assert!(r.output.contains("  1: needle three"), "{}", r.output);
}

#[tokio::test]
async fn a_single_file_path_searches_that_file() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle three", "path": "sub/mid.txt"}),
    )
    .await;
    assert!(!r.is_error);
    assert_eq!(r.output, "sub/mid.txt:\n  1: needle three\n");
}

#[tokio::test]
async fn context_preserves_match_markers_for_adjacent_matches() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "top\nmatch1\nmatch2\nbottom\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "match", "context": 1}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output,
        "f.txt:\n  1- top\n  2: match1\n  3: match2\n  4- bottom\n"
    );

    std::fs::write(dir.path().join("g.txt"), "match1\na\nb\nc\nmatch2\n").unwrap();
    let r2 = run(
        dir.path(),
        serde_json::json!({"pattern": "match", "path": "g.txt", "context": 1}),
    )
    .await;
    assert!(!r2.is_error, "{}", r2.output);
    assert_eq!(
        r2.output,
        "g.txt:\n  1: match1\n  2- a\n  --\n  4- c\n  5: match2\n"
    );
}

#[tokio::test]
async fn glob_with_directory_prefix_matches_when_path_is_omitted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("other")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("other/b.rs"), "needle\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"pattern": "needle", "glob": "src/**/*.rs"}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(r.output, "src/a.rs:\n  1: needle\n");
}

/// A tree of small files — `from` directories up to `upto`, fifty files
/// each — for the walk that has to take a while.
fn plant(dir: &std::path::Path, from: usize, upto: usize) {
    for i in from..upto {
        let sub = dir.join(format!("d{i}"));
        std::fs::create_dir_all(&sub).unwrap();
        for j in 0..50 {
            std::fs::write(sub.join(format!("f{j}.txt")), "text\n").unwrap();
        }
    }
}

/// How long an uncancelled walk of the whole tree takes, having grown the
/// tree until that is a measurable while: a stop request timed at a
/// quarter of the walk then lands inside it on any machine, however fast
/// or loaded. Returns the time and the tree's file count.
async fn a_walk_that_takes_a_while(
    dir: &std::path::Path,
    args: impl Fn(usize) -> Value,
) -> (std::time::Duration, usize) {
    let (mut from, mut dirs) = (0, 200);
    loop {
        plant(dir, from, dirs);
        from = dirs;
        let total = dirs * 50;
        let measured = std::time::Instant::now();
        let complete = run(dir, args(total)).await;
        let walk_time = measured.elapsed();
        assert!(!complete.is_error, "{}", complete.output);
        assert!(
            complete.output.contains(&format!("{total} file(s)")),
            "the uncancelled walk covers the tree: {}",
            complete.output
        );
        if walk_time >= std::time::Duration::from_millis(200) || dirs >= 3200 {
            return (walk_time, total);
        }
        dirs *= 2;
    }
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

/// Esc before the call: the search never starts, and the answer says so
/// as an error rather than as a confident miss.
#[tokio::test]
async fn a_cancelled_search_reports_aborted_without_searching() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run_with(
        dir.path(),
        serde_json::json!({"pattern": "needle", "path": "."}),
        &Cancel::new(Arc::new(AtomicBool::new(true))),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("aborted by user"), "{}", r.output);
}

/// Esc mid-walk: a stop request raised while the search is in flight ends
/// it. The files-searched count in the answer is the proof the walk really
/// stopped early.
#[tokio::test]
async fn a_cancelled_walk_stops_promptly_and_says_it_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    // No pattern matches, so the only thing the walk reports is scope.
    let args = |max: usize| serde_json::json!({"pattern": "no-such-needle", "path": ".", "max_results": max});
    let (walk_time, total) = a_walk_that_takes_a_while(dir.path(), args).await;

    let (cancel, setter) = stop_after(walk_time / 4);
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        run_with(dir.path(), args(total + 10), &cancel),
    )
    .await
    .expect("a cancelled walk must not run on to the end of the tree");
    setter.join().unwrap();

    assert!(r.is_error, "a stopped search is an error: {}", r.output);
    assert!(r.output.contains("aborted by user"), "{}", r.output);
    assert!(r.output.contains("partial"), "{}", r.output);
    let searched: usize = r
        .output
        .split("after ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("the aborted result names the scope it covered");
    assert!(
        searched > 0 && searched < total,
        "the walk must stop partway, covered {searched} of {total}"
    );
}
