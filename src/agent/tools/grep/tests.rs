use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    GrepTool::new(dir.to_path_buf()).execute(&args).await
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
async fn matches_across_a_tree_sorted_with_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    tree(dir.path());

    let r = run(dir.path(), serde_json::json!({"pattern": "needle"})).await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output,
        "sub/deep/low.txt:1: the needle four\n\
         sub/mid.txt:1: needle three\n\
         top.txt:1: alpha needle one\n\
         top.txt:3: needle two\n"
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
    assert_eq!(
        r.output,
        "a.rs-1- one\na.rs:2: AlphaNeedle\na.rs-3- three\n"
    );

    let r_fixed = run(
        dir.path(),
        serde_json::json!({"pattern": "foo[0]", "fixed_strings": true}),
    )
    .await;
    assert!(!r_fixed.is_error);
    assert_eq!(r_fixed.output, "a.rs:4: foo[0]\n");
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
    assert_eq!(r.output, "kept.txt:1: needle\n");
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
    assert!(r.output.contains("mid.txt:1"), "{}", r.output);
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
    assert_eq!(r.output, "sub/mid.txt:1: needle three\n");
}
