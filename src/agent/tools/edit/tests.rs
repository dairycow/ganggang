//! Tests for [`super`]: atomic multi-edit, near-match line-numbered errors,
//! ambiguous line numbers, `replace_all`, compact hunk confirmation, and the
//! 512 KiB cap.

use super::*;

async fn edit_file(dir: &std::path::Path, args: Value) -> ToolResult {
    EditTool::new(dir.to_path_buf())
        .execute(&args, &Cancel::none())
        .await
}

fn written(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn applies_multiple_edits_atomically_with_hunk_preview() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "fn main() {\n    todo!()\n}\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "todo!()", "new_text": "println!(\"hi\")"},
            {"old_text": "fn main", "new_text": "pub fn main"}
        ]}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.starts_with("applied 2 edit(s) to "));
    assert!(r.output.contains("@@ line 2 (-1 +1) @@"), "{}", r.output);
    assert!(r.output.contains("-todo!()"), "{}", r.output);
    assert!(r.output.contains("+println!(\"hi\")"), "{}", r.output);
    assert!(r.output.contains("@@ line 1 (-1 +1) @@"), "{}", r.output);
    assert_eq!(written(&f), "pub fn main() {\n    println!(\"hi\")\n}\n");
}

#[tokio::test]
async fn a_miss_leaves_the_file_untouched_and_reports_nearest_match() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(
        &f,
        "alpha beta\nfn compute(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\nomega\n",
    )
    .unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "alpha", "new_text": "A"},
            {"old_text": "fn compute(x: i32) -> i32 {\n  let y = x + 1;\n  y * 2\n}", "new_text": "G"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("edit 2 failed"));
    assert!(r.output.contains("not found"));
    assert!(
        r.output.contains("closest match around lines 2-5:"),
        "{}",
        r.output
    );
    assert!(r.output.contains("2| fn compute(x: i32)"), "{}", r.output);
    assert_eq!(
        written(&f),
        "alpha beta\nfn compute(x: i32) -> i32 {\n    let y = x + 1;\n    y * 2\n}\nomega\n",
        "first edit must not persist"
    );
}

#[tokio::test]
async fn unrelated_miss_does_not_fabricate_nearest_match() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "alpha beta\ngamma delta\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "completely_different_symbol_xyz_12345", "new_text": "z"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(!r.output.contains("closest match"), "{}", r.output);
}

#[tokio::test]
async fn an_ambiguous_match_is_an_error_with_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "x\na\nx\nb\nx\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "x", "new_text": "y"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("found 3 times"));
    assert!(r.output.contains("lines 1, 3, 5"), "{}", r.output);
    assert_eq!(written(&f), "x\na\nx\nb\nx\n");
}

#[tokio::test]
async fn missing_file_is_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "nope.txt", "edits": [
            {"old_text": "a", "new_text": "b"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("cannot read"));
}

#[tokio::test]
async fn edits_over_50kib_succeed_and_over_512kib_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let medium = format!("{}target_line\n", "a\n".repeat(30_000)); // > 60 KiB
    std::fs::write(dir.path().join("medium.txt"), &medium).unwrap();
    let r_med = edit_file(
        dir.path(),
        serde_json::json!({"path": "medium.txt", "edits": [
            {"old_text": "target_line", "new_text": "updated_line"}
        ]}),
    )
    .await;
    assert!(!r_med.is_error, "{}", r_med.output);
    assert!(written(&dir.path().join("medium.txt")).contains("updated_line"));

    let big = "x".repeat(MAX_BYTES + 1);
    std::fs::write(dir.path().join("big.txt"), &big).unwrap();
    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "big.txt", "edits": [
            {"old_text": "x", "new_text": "y"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("exceeds"));
    assert_eq!(
        std::fs::read(dir.path().join("big.txt")).unwrap().len(),
        big.len()
    );
}

#[tokio::test]
async fn huge_hunk_preview_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let old_block: String = (1..=20).map(|i| format!("old {i}\n")).collect();
    let new_block: String = (1..=20).map(|i| format!("new {i}\n")).collect();
    std::fs::write(dir.path().join("h.txt"), &old_block).unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "h.txt", "edits": [
            {"old_text": old_block, "new_text": new_block}
        ]}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("more lines)"), "{}", r.output);
    assert!(r.output.contains("-old 1"), "{}", r.output);
    assert!(r.output.contains("+new 1"), "{}", r.output);
}

#[tokio::test]
async fn empty_old_text_blank_miss_many_ambiguous_and_long_line_cap() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("edge.txt");
    let mut content = String::new();
    for _ in 0..12 {
        content.push_str("dup\n\n");
    }
    let long = "z".repeat(400);
    content.push_str(&format!("{long}\n"));
    std::fs::write(&f, &content).unwrap();

    let empty_err = edit_file(
        dir.path(),
        serde_json::json!({"path": "edge.txt", "edits": [{"old_text": "", "new_text": "x"}]}),
    )
    .await;
    assert!(empty_err.is_error);
    assert!(empty_err.output.contains("old_text must not be empty"));

    let blank_miss = edit_file(
        dir.path(),
        serde_json::json!({"path": "edge.txt", "edits": [{"old_text": "   \n   ", "new_text": "x"}]}),
    )
    .await;
    assert!(blank_miss.is_error);
    assert!(!blank_miss.output.contains("closest match"));

    let many_ambig = edit_file(
        dir.path(),
        serde_json::json!({"path": "edge.txt", "edits": [{"old_text": "dup", "new_text": "x"}]}),
    )
    .await;
    assert!(many_ambig.is_error);
    assert!(many_ambig.output.contains(", …;"), "{}", many_ambig.output);

    let ok_long = edit_file(
        dir.path(),
        serde_json::json!({"path": "edge.txt", "edits": [{"old_text": long, "new_text": "short"}]}),
    )
    .await;
    assert!(!ok_long.is_error);
    assert!(ok_long.output.contains('…'), "{}", ok_long.output);
}

#[tokio::test]
async fn replace_all_rewrites_every_occurrence_and_reports_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "x\na\nx\nb\nx\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "x", "new_text": "y", "replace_all": true}
        ]}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(
        r.output.contains("@@ line 1 (3 occurrence(s), -1 +1) @@"),
        "{}",
        r.output
    );
    assert_eq!(written(&f), "y\na\ny\nb\ny\n");
}

#[tokio::test]
async fn replace_all_with_no_match_fails_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "a\nb\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "a", "new_text": "A"},
            {"old_text": "nope", "new_text": "N", "replace_all": true}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("edit 2 failed"), "{}", r.output);
    assert!(r.output.contains("not found"), "{}", r.output);
    assert_eq!(written(&f), "a\nb\n", "first edit must not persist");
}

#[tokio::test]
async fn an_ambiguous_match_error_suggests_replace_all() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "x\na\nx\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "x", "new_text": "y"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(
        r.output
            .contains("pass replace_all: true to replace all 2 occurrences"),
        "{}",
        r.output
    );
    assert_eq!(written(&f), "x\na\nx\n");
}

#[tokio::test]
async fn a_miss_that_differs_only_in_indentation_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "fn main() {\n    body();\n}\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "fn main() {\n  body();\n}", "new_text": "G"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(
        r.output
            .contains("first difference at line 2 (whitespace differs)"),
        "{}",
        r.output
    );
    assert!(
        r.output
            .contains(r#"expected "  body();" vs actual "    body();""#),
        "{}",
        r.output
    );
    assert_eq!(written(&f), "fn main() {\n    body();\n}\n");
}

#[tokio::test]
async fn unknown_field_inside_edit_item_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, "a\n").unwrap();

    let r = edit_file(
        dir.path(),
        serde_json::json!({"path": "f.txt", "edits": [
            {"old_text": "a", "new_text": "b", "oldText": "c"}
        ]}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("invalid arguments"), "{}", r.output);
    assert_eq!(written(&f), "a\n");
}
