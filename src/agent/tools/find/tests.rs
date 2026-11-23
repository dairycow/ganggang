use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    FindTool::new(dir.to_path_buf()).execute(&args).await
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
