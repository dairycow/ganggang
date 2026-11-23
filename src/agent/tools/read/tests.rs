use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    ReadTool::new(dir.to_path_buf()).execute(&args).await
}

#[tokio::test]
async fn reads_a_file_with_line_numbers_and_respects_offset_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "f.txt"})).await;
    assert!(!r.is_error);
    assert_eq!(r.output.trim(), "1| a\n2| b\n3| c\n4| d");

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": 2, "limit": 2}),
    )
    .await;
    assert_eq!(r.output.trim(), "2| b\n3| c");
}

#[tokio::test]
async fn offset_pages_cleanly_past_50_kib() {
    let dir = tempfile::tempdir().unwrap();
    // ~120 KiB file: 3000 lines * ~41 bytes each.
    let body: String = (1..=3000)
        .map(|i| format!("line-{i:04}-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n"))
        .collect();
    assert!(body.len() > 100 * 1024);
    std::fs::write(dir.path().join("large.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "large.txt", "offset": 2500, "limit": 3}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output.trim(),
        "2500| line-2500-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n\
         2501| line-2501-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n\
         2502| line-2502-padding-xxxxxxxxxxxxxxxxxxxxxxxx"
    );
}

#[tokio::test]
async fn offset_past_eof_reports_line_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": 10}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("past end of file (2 lines)"));
}

#[tokio::test]
async fn truncates_at_2000_lines_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..2500).map(|i| format!("line{i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), body).unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "big.txt"})).await;
    assert!(!r.is_error);
    assert!(r.output.contains("2000| line1999"));
    assert!(!r.output.contains("2001| line2000"));
    assert!(r.output.contains("showing lines 1-2000 of 2500"));
    assert!(r.output.contains("offset=2001"));
}

#[tokio::test]
async fn truncates_at_50_kib_on_a_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let unit = "é".repeat(64);
    std::fs::write(dir.path().join("wide.txt"), unit.repeat(1000)).unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "wide.txt"})).await;
    assert!(!r.is_error);
    assert!(r.output.contains("truncated"));
    assert!(r.output.len() < MAX_BYTES + 200);
}

#[tokio::test]
async fn missing_file_is_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(dir.path(), serde_json::json!({"path": "nope.txt"})).await;
    assert!(r.is_error);
    assert!(r.output.contains("cannot read"));
}

#[tokio::test]
async fn an_endless_file_returns_the_cap_quickly_instead_of_hanging() {
    let r = run(
        std::path::Path::new("/dev"),
        serde_json::json!({"path": "zero"}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("truncated"));
}
